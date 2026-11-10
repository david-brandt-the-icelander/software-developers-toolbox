#!/usr/bin/env python3
"""fix_network.py - diagnose and progressively repair macOS Wi-Fi / DNS / routing / VPN.

Uses only the stock Python 3 and tools that ship with macOS.

  diagnose   read-only: find the failing layer, save a timestamped log
  fix        diagnose, then repair from the failing layer upward, retesting after each step
  recover    emergency: make sure Wi-Fi power is on (also what the staged watchdog runs)

Safety model
  * Everything is logged under ~/.local/state/fix-network/<timestamp>/ (never in a repo).
  * Original settings are recorded in original.json before any change.
  * Before Wi-Fi is cycled, a detached watchdog (recover.sh) is started that turns Wi-Fi back
    on by itself, so a dropped session cannot strand the Mac offline.
  * It never reboots, touches Docker, deletes network preferences/credentials/profiles,
    substitutes public DNS, or alters routers. Broad steps are opt-in flags.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime

IS_TTY = sys.stdout.isatty()
IS_MAC = sys.platform == "darwin"
STATE_ROOT = os.environ.get("FIXNET_STATE", os.path.expanduser("~/.local/state/fix-network"))
SENSITIVE = re.compile(r"(pass(word)?|secret|token|psk|private.?key|shared.?secret)", re.I)


def c(code, s):
    return "\033[%sm%s\033[0m" % (code, s) if IS_TTY and not os.environ.get("NO_COLOR") else s


class Session:
    def __init__(self, create=True):
        self.dir = os.path.join(STATE_ROOT, datetime.now().strftime("%Y%m%d-%H%M%S"))
        os.makedirs(self.dir, exist_ok=True)
        os.chmod(self.dir, 0o700)
        self.logpath = os.path.join(self.dir, "diagnostic.log")
        self.original = {}
        self.changes = []

    def log(self, msg="", show=True):
        line = "[%s] %s" % (datetime.now().strftime("%H:%M:%S"), msg)
        with open(self.logpath, "a") as f:
            f.write("\n".join(l for l in line.splitlines() if not SENSITIVE.search(l)) + "\n")
        if show:
            print(msg, flush=True)

    def save_original(self):
        with open(os.path.join(self.dir, "original.json"), "w") as f:
            json.dump(self.original, f, indent=2)


def sh(cmd, timeout=15, sudo=False):
    """Run a command with a bound. Returns (rc, combined output)."""
    if sudo and os.geteuid() != 0:
        cmd = ["sudo"] + ([] if sys.stdin.isatty() else ["-n"]) + cmd
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout + r.stderr).strip()
    except subprocess.TimeoutExpired:
        return 124, "(timed out after %ss)" % timeout
    except OSError as e:
        return 127, str(e)


# ------------------------------------------------------------------ discovery


def discover(S):
    info = {}
    info["macos"] = sh(["sw_vers", "-productVersion"])[1]
    _, order = sh(["networksetup", "-listnetworkserviceorder"])
    info["service_order"] = order
    wifi_dev = wifi_service = None
    m = re.search(r"\(\d+\)\s+(.+)\n\(Hardware Port: (?:Wi-Fi|AirPort), Device: (\w+)\)", order)
    if m:
        wifi_service, wifi_dev = m.group(1).strip(), m.group(2)
    info["wifi_device"], info["wifi_service"] = wifi_dev, wifi_service
    # default route owner (may be a VPN)
    _, rt = sh(["route", "-n", "get", "default"])
    mm = re.search(r"interface:\s*(\S+)", rt)
    info["default_route_if"] = mm.group(1) if mm else None
    info["default_route_is_physical"] = info["default_route_if"] == wifi_dev
    _, nc = sh(["scutil", "--nc", "list"])
    info["vpn_list"] = nc
    info["vpns"] = [
        {"name": re.search(r'"([^"]+)"', l).group(1), "connected": "(Connected)" in l}
        for l in nc.splitlines() if "(Connected)" in l or "(Disconnected)" in l if re.search(r'"([^"]+)"', l)
    ]
    return info


def record_original(S, info):
    o = S.original
    o["discover"] = info
    svc, dev = info["wifi_service"], info["wifi_device"]
    if svc:
        for key, cmd in (("getinfo", ["-getinfo", svc]), ("dns", ["-getdnsservers", svc]),
                         ("search", ["-getsearchdomains", svc]), ("web_proxy", ["-getwebproxy", svc]),
                         ("secure_proxy", ["-getsecurewebproxy", svc]), ("pac", ["-getautoproxyurl", svc]),
                         ("auto_discovery", ["-getproxyautodiscovery", svc])):
            o[key] = sh(["networksetup"] + cmd)[1]
    o["routes"] = sh(["netstat", "-rn"])[1]
    o["scutil_dns"] = sh(["scutil", "--dns"])[1]
    o["scutil_proxy"] = sh(["scutil", "--proxy"])[1]
    if dev:
        o["ifconfig"] = sh(["ifconfig", dev])[1]
        o["wifi_power"] = sh(["networksetup", "-getairportpower", dev])[1]
        o["wifi_network"] = sh(["networksetup", "-getairportnetwork", dev])[1]
    S.save_original()
    S.log("original settings saved to %s" % os.path.join(S.dir, "original.json"), show=False)


# ------------------------------------------------------------------ tests


def resolve(name, timeout=6):
    """System resolver path (getaddrinfo -> mDNSResponder), bounded by a thread."""
    out = {}

    def w():
        try:
            out["r"] = socket.getaddrinfo(name, None)[0][4][0]
        except OSError as e:
            out["e"] = str(e)

    t = threading.Thread(target=w, daemon=True)
    t.start()
    t.join(timeout)
    return out.get("r"), out.get("e", "timeout" if t.is_alive() else "")


def tcp(host, port, timeout=5):
    try:
        with socket.create_connection((host, port), timeout):
            return True
    except OSError:
        return False


def health(S, info, quiet=False):
    """Run layered tests. Returns dict with per-layer results and a 'verdict' naming the failing layer."""
    dev, h = info["wifi_device"], {}
    pw = sh(["networksetup", "-getairportpower", dev])[1] if dev else ""
    h["wifi_on"] = "On" in pw
    ip = sh(["ipconfig", "getifaddr", dev])[1] if dev else ""
    h["ip"] = ip if re.match(r"^\d+\.\d+\.\d+\.\d+$", ip) else ""
    h["self_assigned"] = h["ip"].startswith("169.254.")
    gw = sh(["ipconfig", "getoption", dev, "router"])[1] if dev else ""
    h["gateway"] = gw if re.match(r"^\d+\.\d+\.\d+\.\d+$", gw) else ""
    h["gateway_ok"] = bool(h["gateway"]) and sh(["ping", "-c", "2", "-t", "4", h["gateway"]], 10)[0] == 0
    rc, body = sh(["curl", "-sS", "--max-time", "8", "http://captive.apple.com/hotspot-detect.html"], 12)
    h["internet_http"] = rc == 0
    h["captive"] = rc == 0 and "Success" not in body
    h["https_ok"] = sh(["curl", "-sS", "-o", "/dev/null", "--max-time", "8", "https://www.apple.com/"], 12)[0] == 0
    addr, err = resolve("www.apple.com")
    h["dns_ok"] = bool(addr)
    # IP-literal HTTPS-less reachability, to separate DNS from routing
    h["ip_reachable"] = tcp("1.1.1.1", 443, 4)
    if not h["wifi_on"]:
        v = "wifi-off"
    elif not h["ip"] or h["self_assigned"]:
        v = "no-dhcp-lease"
    elif not h["gateway_ok"] and not h["ip_reachable"]:
        v = "gateway-unreachable"
    elif h["captive"]:
        v = "captive-portal"
    elif not h["ip_reachable"] and not h["https_ok"]:
        v = "upstream-or-routing"
    elif not h["dns_ok"]:
        v = "dns"
    elif not h["https_ok"]:
        v = "https-proxy-or-tls"
    else:
        v = "healthy"
    h["verdict"] = v
    if not quiet:
        S.log("  wifi_on=%s ip=%s gateway=%s gw_ping=%s captive=%s ip_reach=%s dns=%s https=%s => %s"
              % (h["wifi_on"], h["ip"] or "-", h["gateway"] or "-", h["gateway_ok"], h["captive"],
                 h["ip_reachable"], h["dns_ok"], h["https_ok"], c("1", v)))
    else:
        S.log("health: " + json.dumps(h), show=False)
    return h


def vpn_report(S, info):
    S.log(c("1", "VPN"))
    S.log("  default route interface: %s%s" % (info["default_route_if"],
          "" if info["default_route_is_physical"] else "  (NOT the Wi-Fi device - a VPN/other interface owns it)"))
    if not info["vpns"]:
        S.log("  no scutil-managed VPN services (third-party clients need their own CLI/UI)")
    for v in info["vpns"]:
        S.log("  %s: %s" % (v["name"], "connected" if v["connected"] else "disconnected"))
    doms = sorted(set(re.findall(r"domain\s+:\s+(\S+)", sh(["scutil", "--dns"])[1])))
    doms = [d for d in doms if not d.endswith(("arpa", "local"))]
    if doms:
        S.log("  scoped/split DNS domains: " + ", ".join(doms[:10]))
    return doms


def verify_internal(S, host_arg, doms):
    if not host_arg:
        S.log("  internal verification INCOMPLETE: pass --internal-host HOST[:PORT]"
              + (" (candidate domains above)" if doms else ""))
        return None
    host, _, port = host_arg.partition(":")
    addr, err = resolve(host)
    S.log("  internal name %s -> %s" % (host, addr or "FAILED (%s)" % err))
    ok = bool(addr)
    if ok and port:
        ok = tcp(host, int(port), 6)
        S.log("  tcp %s:%s %s" % (host, port, "open" if ok else "FAILED"))
    if addr:
        _, rt = sh(["route", "-n", "get", addr])
        mm = re.search(r"interface:\s*(\S+)", rt)
        S.log("  route to %s uses interface %s" % (addr, mm.group(1) if mm else "?"))
    return ok


# ------------------------------------------------------------------ recovery


def stage_recovery(S, dev, delay):
    """Write recover.sh and start a detached watchdog that re-enables Wi-Fi regardless of this process."""
    path = os.path.join(S.dir, "recover.sh")
    with open(path, "w") as f:
        f.write("#!/bin/sh\n# Re-enable Wi-Fi no matter what. Log: %s\n" % S.logpath)
        f.write("sleep %d\n" % delay)
        f.write("/usr/sbin/networksetup -setairportpower %s on\n" % shlex.quote(dev))
        f.write("echo \"$(date) watchdog ensured Wi-Fi on\" >> %s\n" % shlex.quote(S.logpath))
    os.chmod(path, 0o700)
    p = subprocess.Popen(["/bin/sh", path], start_new_session=True, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    S.log("recovery watchdog staged (pid %d): Wi-Fi forced on in %ds. Manual: networksetup -setairportpower %s on"
          % (p.pid, delay, dev))
    return p


def cycle_wifi(S, dev):
    wd = stage_recovery(S, dev, 45)
    try:
        S.log("  cycling Wi-Fi radio ...")
        sh(["networksetup", "-setairportpower", dev, "off"])
        time.sleep(3)
    finally:
        sh(["networksetup", "-setairportpower", dev, "on"])
    S.changes.append("cycled Wi-Fi radio on %s" % dev)
    for _ in range(20):
        time.sleep(2)
        if sh(["ipconfig", "getifaddr", dev])[1].count(".") == 3:
            break
    if wd.poll() is None and sh(["networksetup", "-getairportpower", dev])[1].endswith("On"):
        wd.kill()  # Wi-Fi is back; watchdog no longer needed


# ------------------------------------------------------------------ repair


def dhcp_configured(S):
    return "DHCP Configuration" in S.original.get("getinfo", "")


def fix(S, info, args):
    dev, svc = info["wifi_device"], info["wifi_service"]
    h = health(S, info)
    if h["verdict"] == "healthy" and not args.force:
        S.log(c("32", "Internet already healthy; no repair needed (use --force to run steps anyway)."))
        return h

    # VPN may be the cause: note it, bring it down only after cheaper steps fail
    steps = []
    if h["verdict"] in ("dns", "https-proxy-or-tls", "upstream-or-routing") or args.force:
        steps.append(("flush DNS cache", lambda: (sh(["dscacheutil", "-flushcache"], sudo=True),
                                                  sh(["killall", "-HUP", "mDNSResponder"], sudo=True))))
    gw = h["gateway"]
    if gw and (args.force or h["verdict"] in ("gateway-unreachable", "upstream-or-routing")):
        steps.append(("clear ARP entry for gateway", lambda: sh(["arp", "-d", gw], sudo=True)))
    if dev and dhcp_configured(S) and (args.force or h["verdict"] in (
            "no-dhcp-lease", "gateway-unreachable", "captive-portal", "upstream-or-routing")):
        steps.append(("renew DHCP lease", lambda: sh(["ipconfig", "set", dev, "DHCP"], 20)))
    if dev:
        steps.append(("cycle Wi-Fi (watchdog-protected)", lambda: cycle_wifi(S, dev)))
    if args.fix_proxy and svc:
        steps.append(("disable unreachable proxies", lambda: stale_proxy(S, svc)))
    if args.allow_route_flush:
        steps.append(("route -n flush (broad)", lambda: sh(["route", "-n", "flush"], sudo=True)))

    vpns_down = []
    for name, fn in steps:
        if h["verdict"] == "healthy":
            break
        S.log(c("36", "-> " + name))
        try:
            fn()
        except Exception as e:  # keep going; log the failure
            S.log("   step failed: %s" % e)
        S.changes.append(name)
        time.sleep(4)
        h = health(S, info)
        # if still failing with a VPN up, test without it (once)
        if h["verdict"] != "healthy" and not vpns_down and any(v["connected"] for v in info["vpns"]) and args.vpn:
            for v in info["vpns"]:
                if v["connected"]:
                    S.log(c("36", "-> disconnecting VPN %s to test plain internet" % v["name"]))
                    sh(["scutil", "--nc", "stop", v["name"]], 30)
                    vpns_down.append(v["name"])
            time.sleep(5)
            h = health(S, info)

    if h["verdict"] == "healthy":
        S.log(c("32", "Internet restored."))
    else:
        S.log(c("31", "Still failing at layer: %s" % h["verdict"]))
        advice = {
            "captive-portal": "Open a browser and complete the Wi-Fi sign-in page.",
            "upstream-or-routing": "Gateway answers but the internet does not: likely ISP/router outage - check the router.",
            "gateway-unreachable": "Router unreachable: check signal/AP; consider restarting the router (not done automatically).",
            "no-dhcp-lease": "No lease: the router's DHCP may be down or the Wi-Fi password changed.",
        }.get(h["verdict"])
        if advice:
            S.log("  " + advice)

    for name in vpns_down:  # reconnect what we took down
        S.log(c("36", "-> reconnecting VPN %s" % name))
        sh(["scutil", "--nc", "start", name], 30)
        for _ in range(15):
            time.sleep(2)
            first = (sh(["scutil", "--nc", "status", name])[1].splitlines() or [""])[0]
            if first.strip() == "Connected":
                break
    return h


def stale_proxy(S, svc):
    for kind, get, setter in (("web", "-getwebproxy", "-setwebproxystate"),
                              ("secure", "-getsecurewebproxy", "-setsecurewebproxystate")):
        out = sh(["networksetup", get, svc])[1]
        if "Enabled: Yes" in out:
            host = re.search(r"Server:\s*(\S+)", out).group(1)
            port = int(re.search(r"Port:\s*(\d+)", out).group(1) or 0)
            if not tcp(host, port, 3):
                S.log("   %s proxy %s:%s unreachable; disabling (original recorded; re-enable: "
                      "networksetup %s %r on)" % (kind, host, port, setter, svc))
                sh(["networksetup", setter, svc, "off"])


# ------------------------------------------------------------------ commands


def prepare(args, label):
    if not IS_MAC:
        print("error: fix_network.py works on macOS only", file=sys.stderr)
        sys.exit(2)
    S = Session()
    S.log(c("1", "fix_network %s" % label) + "  (log: %s)" % S.logpath)
    info = discover(S)
    S.log("macOS %s | Wi-Fi device %s | service %s" % (info["macos"], info["wifi_device"], info["wifi_service"]))
    if not info["wifi_device"]:
        print("error: no Wi-Fi hardware port found", file=sys.stderr)
        sys.exit(1)
    record_original(S, info)
    ssid = S.original.get("wifi_network", "")
    S.log("  Wi-Fi network: %s" % (ssid if "Current" in ssid else "(not reported / not associated)"))
    return S, info


def finish(S, h, info, args):
    S.log(c("1", "Summary"))
    S.log("  layer verdict : %s" % h["verdict"])
    S.log("  changes made  : %s" % (", ".join(S.changes) or "none"))
    S.log("  log           : %s" % S.logpath)
    S.log("  original state: %s" % os.path.join(S.dir, "original.json"))
    if h["verdict"] == "healthy" and S.changes:
        S.log("  note: restored by the steps above; the step list is not proof of which one was the cause.")


def cmd_diagnose(args):
    S, info = prepare(args, "diagnose")
    h = health(S, info)
    doms = vpn_report(S, info)
    verify_internal(S, args.internal_host, doms)
    finish(S, h, info, args)


def cmd_fix(args):
    S, info = prepare(args, "fix")
    h = fix(S, info, args)
    S.log(c("1", "Settling 15s, re-checking"))
    time.sleep(15)
    h = health(S, info)
    doms = vpn_report(S, info)
    verify_internal(S, args.internal_host, doms)
    if h["verdict"] == "healthy":
        S.log("  connectivity re-verified after settling")
    finish(S, h, info, args)
    sys.exit(0 if h["verdict"] == "healthy" else 1)


def cmd_recover(args):
    if not IS_MAC:
        sys.exit("macOS only")
    _, out = sh(["networksetup", "-listallhardwareports"])
    devs = re.findall(r"Hardware Port: (?:Wi-Fi|AirPort)\nDevice: (\w+)", out)
    for d in devs:
        print(sh(["networksetup", "-setairportpower", d, "on"])[1] or "Wi-Fi on (%s)" % d)


def main():
    p = argparse.ArgumentParser(
        description="Diagnose and repair macOS Wi-Fi/DNS/routing/VPN, layer by layer.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="examples:\n  fix_network.py diagnose --internal-host wiki.corp.example:443\n"
               "  fix_network.py fix --vpn --internal-host wiki.corp.example:443\n  fix_network.py recover")
    sub = p.add_subparsers(dest="cmd", metavar="command")
    sub.required = True
    for name, fn, hlp in (("diagnose", cmd_diagnose, "read-only diagnosis + log"),
                          ("fix", cmd_fix, "diagnose and repair progressively")):
        s = sub.add_parser(name, help=hlp)
        s.add_argument("--internal-host", metavar="HOST[:PORT]", help="internal service to verify over the VPN")
        if name == "fix":
            s.add_argument("--vpn", action="store_true",
                           help="if still failing, disconnect scutil-managed VPNs to test plain internet, then reconnect")
            s.add_argument("--fix-proxy", action="store_true", help="disable system proxies whose server is unreachable")
            s.add_argument("--allow-route-flush", action="store_true", help="last resort: route -n flush")
            s.add_argument("--force", action="store_true", help="run repair steps even if internet looks healthy")
        s.set_defaults(fn=fn)
    r = sub.add_parser("recover", help="force Wi-Fi power on")
    r.set_defaults(fn=cmd_recover)
    args = p.parse_args()
    try:
        args.fn(args)
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
