// Runs external programs with an argument list, a timeout and an output cap.
using System.Diagnostics;

namespace CodeIndexd;

// Result of one external program run.
public sealed record ProcResult(int ExitCode, string Stdout, string Stderr, bool TimedOut);

// Static process runner; never uses a shell.
public static class Proc
{
    private const int MaxOutputBytes = 8 * 1024 * 1024;

    // Runs a program and returns its output; kills it after the timeout in seconds.
    public static ProcResult Run(string file, IEnumerable<string> args, int timeoutSeconds, string? workingDir = null)
    {
        var psi = new ProcessStartInfo(file)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            RedirectStandardInput = true,
            UseShellExecute = false,
        };
        if (workingDir is not null)
            psi.WorkingDirectory = workingDir;
        foreach (var a in args)
            psi.ArgumentList.Add(a);
        psi.Environment["GIT_OPTIONAL_LOCKS"] = "0";
        Process p;
        try
        {
            p = Process.Start(psi) ?? throw new InvalidOperationException($"cannot start {file}");
        }
        catch (System.ComponentModel.Win32Exception)
        {
            return new ProcResult(127, "", $"{file}: command not found", false);
        }
        using var _ = p;
        p.StandardInput.Close();
        var outTask = ReadCapped(p.StandardOutput);
        var errTask = ReadCapped(p.StandardError);
        var done = p.WaitForExit(timeoutSeconds * 1000);
        if (!done)
            p.Kill(true);
        p.WaitForExit();
        return new ProcResult(done ? p.ExitCode : -1, outTask.Result, errTask.Result, !done);
    }

    // Reads a stream to the end, keeping at most the output cap.
    private static Task<string> ReadCapped(StreamReader reader) => Task.Run(() =>
    {
        var sb = new System.Text.StringBuilder();
        var buf = new char[8192];
        int n;
        while ((n = reader.Read(buf, 0, buf.Length)) > 0)
            if (sb.Length < MaxOutputBytes)
                sb.Append(buf, 0, n);
        return sb.ToString();
    });
}
