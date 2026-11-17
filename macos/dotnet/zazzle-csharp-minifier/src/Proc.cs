// Runs a subprocess with an argument list and a timeout.
using System.Diagnostics;

namespace ZazzleMinifier;

// Starts programs without a shell.
internal static class Proc
{
    // Runs the program and returns its exit code and trimmed standard output; -1 when it cannot start or times out.
    public static (int Code, string Output) Run(string program, IEnumerable<string> args, int timeoutSeconds, string? workingDir = null)
    {
        var info = new ProcessStartInfo(program) { RedirectStandardOutput = true, RedirectStandardError = true, WorkingDirectory = workingDir ?? "" };
        foreach (var arg in args)
            info.ArgumentList.Add(arg);
        try
        {
            using var process = Process.Start(info)!;
            var output = process.StandardOutput.ReadToEndAsync();
            _ = process.StandardError.ReadToEndAsync();
            if (!process.WaitForExit(timeoutSeconds * 1000))
            {
                process.Kill(true);
                return (-1, "");
            }
            return (process.ExitCode, output.Result.Trim());
        }
        catch (System.ComponentModel.Win32Exception)
        {
            return (-1, "");
        }
    }
}
