// Small libc helpers for path canonicalisation.
using System.Runtime.InteropServices;

namespace CodeIndexd;

// Static wrappers over libc.
public static partial class Native
{
    [LibraryImport("libc", EntryPoint = "realpath", StringMarshalling = StringMarshalling.Utf8)]
    private static partial IntPtr RealPathC(string path, IntPtr resolved);

    // Resolves symlinks in a path; returns the input when that fails or on Windows.
    public static string RealPath(string path)
    {
        if (OperatingSystem.IsWindows())
            return path;
        var p = RealPathC(path, IntPtr.Zero);
        if (p == IntPtr.Zero)
            return path;
        var s = Marshal.PtrToStringUTF8(p) ?? path;
        Marshal.FreeHGlobal(p);
        return s;
    }
}
