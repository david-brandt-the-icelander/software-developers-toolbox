// Runs a subprocess with an argument list and a timeout.
using System.Diagnostics;

namespace CSharpMinifier;

// Starts programs without a shell.
internal static class Proc
{
    // Runs the program and returns its exit code and trimmed standard output; -1 when it cannot start or times out.
    public static (int Code, string Output) Run(string program, IEnumerable<string> args, int timeoutSeconds, string? workingDir = null)
    {
        var (code, output, _) = RunWithInput(program, args, timeoutSeconds, workingDir, null);
        return (code, output);
    }

    // Runs the program with text on its standard input and returns its exit code, trimmed standard output and standard error; -1 when it cannot start or times out.
    public static (int Code, string Output, string Error) RunWithInput(string program, IEnumerable<string> args, int timeoutSeconds, string? workingDir, string? input)
    {
        var info = new ProcessStartInfo(program)
        {
            RedirectStandardOutput = true, RedirectStandardError = true, RedirectStandardInput = input != null, WorkingDirectory = workingDir ?? "",
        };
        foreach (var arg in args)
            info.ArgumentList.Add(arg);
        try
        {
            using var process = Process.Start(info)!;
            var output = process.StandardOutput.ReadToEndAsync();
            var error = process.StandardError.ReadToEndAsync();
            if (input != null)
            {
                process.StandardInput.Write(input);
                process.StandardInput.Close();
            }
            if (!process.WaitForExit(timeoutSeconds * 1000))
            {
                process.Kill(true);
                return (-1, "", "timed out");
            }
            return (process.ExitCode, output.Result.Trim(), error.Result.Trim());
        }
        catch (System.ComponentModel.Win32Exception e)
        {
            return (-1, "", e.Message);
        }
    }
}
