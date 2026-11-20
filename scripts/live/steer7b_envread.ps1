# steer7b review: read ONE environment variable of another same-user process (what a tool child
# can do to a bg shim on Windows: OpenProcess(VM_READ|QUERY) + the PEB). Prints the value only.
# usage: powershell -NoProfile -File steer7b_envread.ps1 <pid> <VARNAME>
param([int]$ProcId, [string]$Name)
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class EnvRead {
  [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint a, bool i, int pid);
  [DllImport("kernel32.dll")] static extern bool ReadProcessMemory(IntPtr p, IntPtr addr, byte[] buf, IntPtr n, out IntPtr read);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("ntdll.dll")] static extern int NtQueryInformationProcess(IntPtr h, int c, byte[] pbi, int len, out int ret);
  public static string Get(int pid, string name) {
    IntPtr h = OpenProcess(0x0410, false, pid); // QUERY_INFORMATION | VM_READ
    if (h == IntPtr.Zero) return "ERR open " + Marshal.GetLastWin32Error();
    try {
      byte[] pbi = new byte[48]; int ret;
      if (NtQueryInformationProcess(h, 0, pbi, 48, out ret) != 0) return "ERR nt";
      long peb = BitConverter.ToInt64(pbi, 8); IntPtr r; byte[] b = new byte[8];
      ReadProcessMemory(h, (IntPtr)(peb + 0x20), b, (IntPtr)8, out r); long pp = BitConverter.ToInt64(b, 0);
      ReadProcessMemory(h, (IntPtr)(pp + 0x80), b, (IntPtr)8, out r); long env = BitConverter.ToInt64(b, 0);
      byte[] sz = new byte[8]; ReadProcessMemory(h, (IntPtr)(pp + 0x3F0), sz, (IntPtr)8, out r); long len = BitConverter.ToInt64(sz, 0);
      if (len <= 0 || len > 1<<20) return "ERR size";
      byte[] data = new byte[len]; ReadProcessMemory(h, (IntPtr)env, data, (IntPtr)len, out r);
      string all = Encoding.Unicode.GetString(data);
      foreach (string kv in all.Split('\0')) if (kv.StartsWith(name + "=", StringComparison.OrdinalIgnoreCase)) return kv.Substring(name.Length + 1);
      return "ERR notfound";
    } finally { CloseHandle(h); }
  }
}
"@
[EnvRead]::Get($ProcId, $Name)
