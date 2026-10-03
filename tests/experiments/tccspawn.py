"""Spawn a process that is its OWN TCC-responsible process (macOS), with extra inherited fds.

A child of a shell inherits the shell host's TCC identity (E1): Chrome launched that way cannot
even touch its own protected data folder. `open -a` fixes the attribution but cannot pass the
DevTools pipe. Disclaiming responsibility at spawn time (the private but long-standing
`responsibility_spawnattrs_setdisclaim`, used by Apple's own tools) gives the child its own
identity AND lets us keep fds 3/4 for `--remote-debugging-pipe`. Test harness only.
"""
import ctypes, ctypes.util, os

libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
libsys = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)

posix_spawnattr_t = ctypes.c_void_p
posix_spawn_file_actions_t = ctypes.c_void_p


def spawn_disclaimed(argv, env=None, dup_fds=()):
    """Start argv with its own TCC identity. `dup_fds` = [(src_fd, target_fd), ...]."""
    attr = posix_spawnattr_t()
    fa = posix_spawn_file_actions_t()
    if libc.posix_spawnattr_init(ctypes.byref(attr)) != 0:
        raise OSError(ctypes.get_errno(), "posix_spawnattr_init")
    if libc.posix_spawn_file_actions_init(ctypes.byref(fa)) != 0:
        raise OSError(ctypes.get_errno(), "posix_spawn_file_actions_init")
    disclaim = getattr(libsys, "responsibility_spawnattrs_setdisclaim")
    disclaim.argtypes = [ctypes.c_void_p, ctypes.c_int]
    if disclaim(ctypes.byref(attr), 1) != 0:
        raise OSError("responsibility_spawnattrs_setdisclaim failed")
    for src, dst in dup_fds:
        os.set_inheritable(src, True)  # Python fds are close-on-exec by default
        if src != dst:
            libc.posix_spawn_file_actions_adddup2(ctypes.byref(fa), src, dst)
    c_argv = (ctypes.c_char_p * (len(argv) + 1))(*[a.encode() for a in argv], None)
    e = env if env is not None else dict(os.environ)
    c_env = (ctypes.c_char_p * (len(e) + 1))(*[f"{k}={v}".encode() for k, v in e.items()], None)
    pid = ctypes.c_int()
    rc = libc.posix_spawn(ctypes.byref(pid), argv[0].encode(), ctypes.byref(fa), ctypes.byref(attr), c_argv, c_env)
    libc.posix_spawn_file_actions_destroy(ctypes.byref(fa))
    libc.posix_spawnattr_destroy(ctypes.byref(attr))
    if rc != 0:
        raise OSError(rc, os.strerror(rc))
    return pid.value
