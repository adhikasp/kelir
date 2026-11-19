// winspy: logs every NEW top-level window and every NEW process on the desktop.
//
// Build:  scripts/live/winspy-build.ps1   (uses the .NET Framework csc.exe; no SDK needed)
// Run:    winspy.exe <log-file> [max-seconds=120]      stops at max-seconds or when <log-file>.stop exists
//
// Two window sources (so a window that lives for a few ms is still seen):
//   * a WinEvent hook (EVENT_OBJECT_CREATE / SHOW / DESTROY, out of context) on a message-loop thread
//   * an EnumWindows poll every ~5 ms
// One process source: a Toolhelp32 snapshot every ~5 ms (the command line is read from the PEB the
// first time a process is seen; a process that dies inside one poll has no command line).
// Processes that already existed at start are baseline and never logged. Every line is one record:
//   HH:mm:ss.fff WIN  <CREATE|SHOW|POLL> hwnd=.. class=.. vis=0|1 title="..." pid=.. proc=.. chain=..
//   HH:mm:ss.fff PROC <name> pid=.. ppid=.. chain=.. cmd=...
// A WIN line is a FLASH candidate when it is visible (vis=1) and its class is one of the console/terminal
// classes, or its title mentions bash.exe/kelir/cmd.exe/pwsh. The SUMMARY block at the end counts them.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

static class WinSpy
{
    delegate bool EnumProc(IntPtr h, IntPtr l);
    delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc p, uint pid, uint tid, uint flags);
    [DllImport("user32.dll")] static extern bool GetMessage(out MSG m, IntPtr h, uint a, uint b);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG m);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG m);
    [DllImport("user32.dll")] static extern bool PostThreadMessage(uint tid, uint msg, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("kernel32.dll")] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32FirstW(IntPtr s, ref PE32 e);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32NextW(IntPtr s, ref PE32 e);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern bool ReadProcessMemory(IntPtr p, IntPtr addr, byte[] buf, IntPtr n, out IntPtr read);
    [DllImport("ntdll.dll")] static extern int NtQueryInformationProcess(IntPtr p, int cls, ref PBI pbi, int len, out int ret);

    [StructLayout(LayoutKind.Sequential)] struct MSG { public IntPtr h; public uint m; public IntPtr w, l; public uint t; public int x, y; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct PE32 { public uint size, usage, pid; public IntPtr heap; public uint mod, threads, ppid; public int pri; public uint flags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string exe; }
    [StructLayout(LayoutKind.Sequential)] struct PBI { public IntPtr exit, peb, aff, prio, pid, ppid; }

    static readonly object L = new object();
    static StreamWriter log;
    static readonly Dictionary<uint, string[]> procs = new Dictionary<uint, string[]>(); // pid -> {name, ppid}
    static readonly HashSet<long> seenWin = new HashSet<long>();
    static readonly HashSet<string> seenEv = new HashSet<string>();
    static volatile bool stop;
    static readonly Dictionary<string, int> cnt = new Dictionary<string, int>();
    static int flashVisible, hiddenConsole;
    static readonly List<string> flashLines = new List<string>();
    static readonly string[] interestingProc = { "conhost", "openconsole", "windowsterminal", "bash", "sleep", "kelir", "cmd", "pwsh", "powershell", "wt", "python", "sh", "git", "ls" };
    static readonly string[] flashClass = { "ConsoleWindowClass", "PseudoConsoleWindow", "CASCADIA_HOSTING_WINDOW_CLASS" };

    static string Ts() { return DateTime.Now.ToString("HH:mm:ss.fff"); }
    static void W(string s) { lock (L) { log.WriteLine(s); } }
    static void Bump(string k) { lock (L) { int v; cnt.TryGetValue(k, out v); cnt[k] = v + 1; } }

    static string Chain(uint pid)
    {
        var sb = new StringBuilder(); int guard = 0;
        lock (L)
        {
            while (pid != 0 && guard++ < 8)
            {
                string[] p;
                if (!procs.TryGetValue(pid, out p)) { sb.Append("?(" + pid + ")"); break; }
                if (sb.Length > 0) sb.Append("<-");
                sb.Append(p[0] + "(" + pid + ")");
                uint pp; if (!uint.TryParse(p[1], out pp)) break; pid = pp;
            }
        }
        return sb.ToString();
    }

    static string CmdLine(uint pid)
    {
        IntPtr h = OpenProcess(0x0410, false, pid); // QUERY_INFORMATION | VM_READ
        if (h == IntPtr.Zero) return "";
        try
        {
            var pbi = new PBI(); int rl;
            if (NtQueryInformationProcess(h, 0, ref pbi, Marshal.SizeOf(pbi), out rl) != 0) return "";
            var b = new byte[8]; IntPtr r;
            if (!ReadProcessMemory(h, (IntPtr)(pbi.peb.ToInt64() + 0x20), b, (IntPtr)8, out r)) return ""; // ProcessParameters
            long pp = BitConverter.ToInt64(b, 0);
            var us = new byte[16];
            if (!ReadProcessMemory(h, (IntPtr)(pp + 0x70), us, (IntPtr)16, out r)) return ""; // CommandLine UNICODE_STRING
            int len = BitConverter.ToUInt16(us, 0); long buf = BitConverter.ToInt64(us, 8);
            if (len <= 0 || len > 32000) return "";
            var data = new byte[len];
            if (!ReadProcessMemory(h, (IntPtr)buf, data, (IntPtr)len, out r)) return "";
            return Encoding.Unicode.GetString(data).Replace("\r", " ").Replace("\n", " ");
        }
        catch { return ""; }
        finally { CloseHandle(h); }
    }

    static void ScanProcs(bool baseline)
    {
        IntPtr s = CreateToolhelp32Snapshot(2, 0);
        if (s == (IntPtr)(-1)) return;
        var e = new PE32(); e.size = (uint)Marshal.SizeOf(typeof(PE32));
        var fresh = new List<uint>();
        if (Process32FirstW(s, ref e))
            do
            {
                lock (L)
                {
                    string[] known;
                    if (procs.TryGetValue(e.pid, out known) && known[2] == e.ppid.ToString() + "|" + e.exe) continue;
                    procs[e.pid] = new[] { Path.GetFileNameWithoutExtension(e.exe), e.ppid.ToString(), e.ppid.ToString() + "|" + e.exe };
                }
                if (!baseline) fresh.Add(e.pid);
            } while (Process32NextW(s, ref e));
        CloseHandle(s);
        foreach (uint pid in fresh)
        {
            string name, ppid;
            lock (L) { name = procs[pid][0]; ppid = procs[pid][1]; }
            string cmd = CmdLine(pid);
            Bump("proc:" + name.ToLowerInvariant());
            bool hit = false; string ln = name.ToLowerInvariant();
            foreach (var i in interestingProc) if (ln == i || ln.StartsWith(i + ".")) hit = true;
            W(Ts() + " PROC " + (hit ? "* " : "  ") + name + " pid=" + pid + " ppid=" + ppid + " chain=" + Chain(pid) + " cmd=" + cmd);
        }
    }

    static bool IsFlashy(string cls, string title)
    {
        foreach (var c in flashClass) if (cls == c) return true;
        string t = title.ToLowerInvariant();
        return t.Contains("bash.exe") || t.Contains("kelir") || t.Contains("cmd.exe") || t.Contains("pwsh") || t.Contains("c:\\program files\\git");
    }

    static void Window(IntPtr h, string src)
    {
        if (h == IntPtr.Zero) return;
        var cb = new StringBuilder(256); GetClassName(h, cb, 256);
        var tb = new StringBuilder(512); GetWindowText(h, tb, 512);
        uint pid; GetWindowThreadProcessId(h, out pid);
        bool vis = IsWindowVisible(h);
        string cls = cb.ToString(), title = tb.ToString();
        // each (hwnd, event, visibility) is logged once; a window is "new" if the hook saw it created, or the poll sees it the first time
        string key = h.ToInt64() + "|" + src + "|" + vis;
        lock (L) { if (!seenEv.Add(key)) return; }
        string name; lock (L) { string[] p; name = procs.TryGetValue(pid, out p) ? p[0] : "?"; }
        bool flashy = IsFlashy(cls, title);
        // Not interesting: ordinary hidden helper windows (IME, MSCTFIME, .NET broadcast ...) unless they are console classes.
        if (!flashy && !vis) return;
        if (!flashy && src == "CREATE") return; // a visible window of an unrelated app: logged at SHOW/POLL only
        Bump("win:" + cls + (vis ? ":visible" : ":hidden"));
        string line = Ts() + " WIN  " + src + " hwnd=0x" + h.ToString("x") + " class=" + cls + " vis=" + (vis ? 1 : 0) + " title=\"" + title + "\" pid=" + pid + " proc=" + name + " chain=" + Chain(pid);
        if (flashy)
        {
            lock (L)
            {
                if (vis) { flashVisible++; flashLines.Add(line); } else hiddenConsole++;
            }
            line = "** " + line;
        }
        W(line);
    }

    static void Hook(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time)
    {
        if (idObject != 0 || idChild != 0 || hwnd == IntPtr.Zero) return;
        ScanProcs(false); // owner may be brand new
        Window(hwnd, ev == 0x8000 ? "CREATE" : ev == 0x8002 ? "SHOW" : "DESTROY");
    }

    static void Main(string[] a)
    {
        if (a.Length < 1) { Console.Error.WriteLine("usage: winspy <log> [max-seconds]"); return; }
        string path = a[0]; int max = a.Length > 1 ? int.Parse(a[1]) : 120;
        string stopFile = path + ".stop"; try { File.Delete(stopFile); } catch { }
        log = new StreamWriter(new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.ReadWrite), new UTF8Encoding(false)) { AutoFlush = true };
        ScanProcs(true);
        EnumWindows((h, l) => { lock (L) { seenWin.Add(h.ToInt64()); } return true; }, IntPtr.Zero); // baseline windows
        W(Ts() + " START baseline_procs=" + procs.Count + " baseline_windows=" + seenWin.Count + " max=" + max + "s");

        uint hookTid = 0; var ready = new ManualResetEvent(false);
        WinEventProc keep = Hook; // keep the delegate alive
        var ht = new Thread(() =>
        {
            hookTid = GetCurrentThreadId();
            SetWinEventHook(0x8000, 0x8002, IntPtr.Zero, keep, 0, 0, 0); // CREATE..SHOW, out of context
            SetWinEventHook(0x8001, 0x8001, IntPtr.Zero, keep, 0, 0, 0); // DESTROY
            ready.Set();
            MSG m; while (GetMessage(out m, IntPtr.Zero, 0, 0)) { TranslateMessage(ref m); DispatchMessage(ref m); }
        });
        ht.IsBackground = true; ht.SetApartmentState(ApartmentState.STA); ht.Start(); ready.WaitOne();

        var sw = Stopwatch.StartNew();
        while (!stop && sw.Elapsed.TotalSeconds < max && !File.Exists(stopFile))
        {
            ScanProcs(false);
            EnumWindows((h, l) =>
            {
                bool fresh; lock (L) { fresh = seenWin.Add(h.ToInt64()); }
                if (fresh) Window(h, "POLL");
                return true;
            }, IntPtr.Zero);
            Thread.Sleep(4);
        }
        if (hookTid != 0) PostThreadMessage(hookTid, 0x0012, IntPtr.Zero, IntPtr.Zero); // WM_QUIT
        lock (L)
        {
            W(Ts() + " SUMMARY visible_console_or_terminal_windows=" + flashVisible + " hidden_console_windows=" + hiddenConsole + " seconds=" + (int)sw.Elapsed.TotalSeconds);
            var keys = new List<string>(cnt.Keys); keys.Sort();
            foreach (var k in keys) W("  count " + k + " = " + cnt[k]);
            foreach (var f in flashLines) W("  FLASH " + f);
            W(flashVisible == 0 ? "VERDICT NO-FLASH" : "VERDICT FLASH");
        }
        log.Flush(); log.Close();
        try { File.Delete(stopFile); } catch { }
    }
}
