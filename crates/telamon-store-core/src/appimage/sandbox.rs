//! A seccomp-bpf allowlist for the inspection helper (`telamon-store
//! --appimage-inspect`, see [`super::helper`]).
//!
//! The helper opens the file, reads the ELF headers, hashes the whole file and
//! runs `gpgv` (the one thing that needs `execve` and `open`) while it still
//! can. Then it calls [`enter`], and only after that does it do the complex
//! work on bytes written by a stranger: the squashfs walk, the decompressors
//! (zlib, liblzma, libzstd) and the XML, desktop-entry and icon parsing. From
//! that point the process can read the file it already holds, allocate memory
//! and write its answer to its standard output, and nothing else: no `open`,
//! no `execve`, no sockets, no `ptrace`, no new processes or threads, no
//! `unlink` or `rename`, no `ioctl`, no executable memory. A bug in a parser
//! that gives the file's author control of the process gets them a process
//! that cannot do anything; any other system call kills it (`SIGSYS`).
//!
//! The filter is written by hand (no crate): a small BPF assembler over the
//! `seccomp_data` the kernel hands each system call. x86-64 only: the syscall
//! numbers and the audit architecture are the architecture's; elsewhere
//! [`enter`] says it did nothing and the helper keeps its resource limits and
//! `no_new_privs`.

use std::io;

// ---- the BPF assembler ----

/// The kernel's return values for a filter (`linux/seccomp.h`).
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const RET_ERRNO: u32 = 0x0005_0000;

/// `AUDIT_ARCH_X86_64`: `EM_X86_64` | 64-bit | little endian.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(not(target_arch = "x86_64"))]
const AUDIT_ARCH: u32 = 0;

/// Offsets into `struct seccomp_data { int nr; u32 arch; u64 ip; u64 args[6] }`.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
/// The low 32 bits of argument `i` (little endian). Every argument checked
/// here is an `int` or a flag word: the kernel reads only these bits.
const fn off_arg(i: u32) -> u32 {
    16 + 8 * i
}

// BPF opcodes (`linux/bpf_common.h`).
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
const BPF_JSET_K: u16 = 0x45;
const BPF_RET_K: u16 = 0x06;

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

/// What a rule lets a system call do.
#[derive(Debug, Clone)]
pub enum Rule {
    /// Allowed with any arguments.
    Allow(i64),
    /// Allowed only when the argument `arg` (its low 32 bits) is one of the
    /// values.
    ArgIn(i64, u32, &'static [u32]),
    /// Allowed only when none of the bits of `mask` is set in the argument.
    ArgNoBits(i64, u32, u32),
    /// Allowed only when every listed argument (index, low 32 bits) has its
    /// value.
    ArgsEq(i64, Vec<(u32, u32)>),
    /// Not allowed, but only fails with this errno instead of killing.
    Errno(i64, i32),
}

/// Where a jump goes.
#[derive(Clone, Copy)]
enum To {
    /// The next instruction plus this many.
    Skip(u8),
    Allow,
    Kill,
    Errno(usize),
}

struct Asm {
    ins: Vec<(libc::sock_filter, To, To)>,
    errnos: Vec<i32>,
}

impl Asm {
    fn push(&mut self, code: u16, k: u32, jt: To, jf: To) {
        self.ins.push((stmt(code, k), jt, jf));
    }

    fn errno_label(&mut self, e: i32) -> To {
        let i = self.errnos.iter().position(|x| *x == e).unwrap_or_else(|| {
            self.errnos.push(e);
            self.errnos.len() - 1
        });
        To::Errno(i)
    }
}

/// A finished filter.
pub struct Filter {
    prog: Vec<libc::sock_filter>,
}

impl Filter {
    /// Assembles `rules`: the architecture is checked first (a call made
    /// through another ABI is killed), then each rule in turn, and anything
    /// no rule matches kills the process.
    pub fn new(rules: &[Rule]) -> Result<Filter, String> {
        let mut a = Asm {
            ins: Vec::new(),
            errnos: Vec::new(),
        };
        a.push(BPF_LD_W_ABS, OFF_ARCH, To::Skip(0), To::Skip(0));
        a.push(BPF_JEQ_K, AUDIT_ARCH, To::Skip(1), To::Skip(0));
        a.push(BPF_RET_K, RET_KILL_PROCESS, To::Skip(0), To::Skip(0));
        a.push(BPF_LD_W_ABS, OFF_NR, To::Skip(0), To::Skip(0));
        for rule in rules {
            match rule {
                Rule::Allow(nr) => {
                    a.push(BPF_JEQ_K, *nr as u32, To::Allow, To::Skip(0));
                }
                Rule::Errno(nr, e) => {
                    let to = a.errno_label(*e);
                    a.push(BPF_JEQ_K, *nr as u32, to, To::Skip(0));
                }
                Rule::ArgsEq(nr, wanted) => {
                    // Each argument in turn must match, else Kill; when all
                    // do, the block ends in Allow.
                    let block = u8::try_from(wanted.len() * 2 + 1)
                        .map_err(|_| "too many arguments".to_string())?;
                    a.push(BPF_JEQ_K, *nr as u32, To::Skip(0), To::Skip(block));
                    for (arg, value) in wanted {
                        a.push(BPF_LD_W_ABS, off_arg(*arg), To::Skip(0), To::Skip(0));
                        a.push(BPF_JEQ_K, *value, To::Skip(0), To::Kill);
                    }
                    a.push(BPF_RET_K, RET_ALLOW, To::Skip(0), To::Skip(0));
                }
                Rule::ArgIn(nr, arg, values) => {
                    // nr? no: skip the block. Else load the argument; one
                    // jump to Allow per value; the block ends in Kill so a
                    // mismatch never falls into the next rule with the
                    // argument in the accumulator.
                    let block = u8::try_from(1 + values.len() + 1)
                        .map_err(|_| "too many values".to_string())?;
                    a.push(BPF_JEQ_K, *nr as u32, To::Skip(0), To::Skip(block));
                    a.push(BPF_LD_W_ABS, off_arg(*arg), To::Skip(0), To::Skip(0));
                    for v in *values {
                        a.push(BPF_JEQ_K, *v, To::Allow, To::Skip(0));
                    }
                    a.push(BPF_RET_K, RET_KILL_PROCESS, To::Skip(0), To::Skip(0));
                }
                Rule::ArgNoBits(nr, arg, mask) => {
                    a.push(BPF_JEQ_K, *nr as u32, To::Skip(0), To::Skip(2));
                    a.push(BPF_LD_W_ABS, off_arg(*arg), To::Skip(0), To::Skip(0));
                    a.push(BPF_JSET_K, *mask, To::Kill, To::Allow);
                }
            }
        }
        // The tail: kill, allow, then one return per errno.
        let body = a.ins.len();
        let kill_at = body;
        let allow_at = body + 1;
        let errno_at = |i: usize| body + 2 + i;
        let mut prog = Vec::with_capacity(body + 2 + a.errnos.len());
        for (pos, (ins, jt, jf)) in a.ins.iter().enumerate() {
            let rel = |to: &To| -> Result<u8, String> {
                let target = match to {
                    To::Skip(n) => return Ok(*n),
                    To::Kill => kill_at,
                    To::Allow => allow_at,
                    To::Errno(i) => errno_at(*i),
                };
                u8::try_from(target - (pos + 1)).map_err(|_| "the filter is too long".to_string())
            };
            let mut ins = *ins;
            // The conditional jumps only; a statement has none.
            if ins.code == BPF_JEQ_K || ins.code == BPF_JSET_K {
                ins.jt = rel(jt)?;
                ins.jf = rel(jf)?;
            }
            prog.push(ins);
        }
        prog.push(stmt(BPF_RET_K, RET_KILL_PROCESS));
        prog.push(stmt(BPF_RET_K, RET_ALLOW));
        for e in &a.errnos {
            prog.push(stmt(BPF_RET_K, RET_ERRNO | (*e as u32 & 0xffff)));
        }
        if prog.len() > 4096 {
            return Err("the filter is too long".into());
        }
        Ok(Filter { prog })
    }

    /// Number of BPF instructions.
    pub fn len(&self) -> usize {
        self.prog.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prog.is_empty()
    }

    /// Puts the filter on this process (every thread of it: `TSYNC`), for good:
    /// it cannot be removed, and `no_new_privs` is set so it needs no
    /// privilege. Fails closed: the caller must not go on when this fails.
    pub fn install(&self) -> io::Result<()> {
        const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
        const SECCOMP_FILTER_FLAG_TSYNC: libc::c_ulong = 1;
        let prog = libc::sock_fprog {
            len: u16::try_from(self.prog.len()).map_err(|_| io::Error::other("too long"))?,
            filter: self.prog.as_ptr().cast_mut(),
        };
        // SAFETY: prctl with these arguments only sets a flag on this process.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `prog` and the instructions it points to are alive for the
        // call, and the kernel copies them.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_TSYNC,
                &raw const prog,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

// ---- the inspector's list ----

/// Whether this build has the inspector's filter (x86-64).
pub const AVAILABLE: bool = cfg!(target_arch = "x86_64");

/// The rules of the inspector's allowlist. Each entry says why it is there.
/// `pid` is this process's id (for `tgkill`).
#[cfg(target_arch = "x86_64")]
pub fn inspector_rules(pid: u32) -> Vec<Rule> {
    use libc::*;
    // Statics: the rules point at their value lists.
    static STD_OUT_ERR: [u32; 2] = [1, 2];
    // `F_DUPFD_CLOEXEC`: how `File::try_clone` duplicates a descriptor.
    // `F_GETFD`: reads a descriptor's close-on-exec flag; std asks it before
    // every `close` in builds with debug assertions.
    static FCNTL_CMDS: [u32; 2] = [F_DUPFD_CLOEXEC as u32, F_GETFD as u32];
    // What `strace -f` shows the helper doing after the filter is on, with
    // the work that only a bigger file adds (freeing large blocks). Single
    // threaded, so no futex, no clone, no thread-exit calls.
    vec![
        // Reading the file it holds: the squashfs reader seeks and reads,
        // the ELF and signature reads are `pread`.
        Rule::Allow(SYS_read),
        Rule::Allow(SYS_pread64),
        Rule::Allow(SYS_lseek),
        // The answer, and a panic's message: standard output and error only.
        Rule::ArgIn(SYS_write, 0, &STD_OUT_ERR),
        Rule::Allow(SYS_close),
        // `File::try_clone` (the squashfs reader gets its own handle):
        // duplicating a descriptor it already has, and the flag read; nothing
        // else (no `F_SETFL`, no locks).
        Rule::ArgIn(SYS_fcntl, 1, &FCNTL_CMDS),
        // Memory for the allocator, the decompressors and the parsers: the
        // heap, and anonymous blocks that grow, shrink and go back. Nothing
        // can be made executable.
        Rule::Allow(SYS_brk),
        Rule::ArgNoBits(SYS_mmap, 2, PROT_EXEC as u32),
        Rule::Allow(SYS_munmap),
        Rule::Allow(SYS_mremap),
        Rule::Allow(SYS_madvise),
        // The seed of every `HashMap` (and glibc's setup of it, which blocks
        // signals for a moment).
        Rule::Allow(SYS_getrandom),
        Rule::Allow(SYS_rt_sigprocmask),
        // Ending, and what a panic or a failed allocation ends in: `abort()`
        // resets the signal's handler and signals this process with SIGABRT.
        Rule::Allow(SYS_exit_group),
        Rule::Allow(SYS_rt_sigaction),
        Rule::Allow(SYS_rt_sigreturn),
        Rule::Allow(SYS_getpid),
        Rule::Allow(SYS_gettid),
        // ... which signals itself: this process, `SIGABRT`, nothing else.
        Rule::ArgsEq(SYS_tgkill, vec![(0, pid), (2, SIGABRT as u32)]),
    ]
}

#[cfg(not(target_arch = "x86_64"))]
pub fn inspector_rules(_pid: u32) -> Vec<Rule> {
    Vec::new()
}

/// What [`enter`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entered {
    /// The filter is on.
    Yes,
    /// Not on this architecture; the process goes on without it.
    Skipped,
}

/// Puts the inspector's filter on this process. `Err` when it could not be
/// installed: the caller must then not read the file's contents.
pub fn enter() -> Result<Entered, String> {
    if !AVAILABLE {
        log::info!("the inspection sandbox is only built for x86-64; going on without it");
        return Ok(Entered::Skipped);
    }
    let filter = Filter::new(&inspector_rules(std::process::id()))?;
    filter
        .install()
        .map(|()| Entered::Yes)
        .map_err(|e| format!("the sandbox could not be made ({})", e.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the program on a `seccomp_data` like the kernel does.
    fn run(prog: &Filter, arch: u32, nr: u32, args: [u32; 6]) -> u32 {
        let load = |off: u32| -> u32 {
            match off {
                OFF_NR => nr,
                OFF_ARCH => arch,
                o if (16..64).contains(&o) && (o - 16) % 8 == 0 => args[((o - 16) / 8) as usize],
                o if (20..68).contains(&o) && (o - 20) % 8 == 0 => 0,
                _ => panic!("load at {off}"),
            }
        };
        let (mut a, mut pc) = (0u32, 0usize);
        for _ in 0..prog.prog.len() + 1 {
            let i = prog.prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => a = load(i.k),
                BPF_JEQ_K => pc += usize::from(if a == i.k { i.jt } else { i.jf }),
                BPF_JSET_K => pc += usize::from(if a & i.k != 0 { i.jt } else { i.jf }),
                BPF_RET_K => return i.k,
                c => panic!("opcode {c:#x}"),
            }
        }
        panic!("ran off the program");
    }

    const ARCH: u32 = AUDIT_ARCH;

    fn rules() -> Vec<Rule> {
        static VALUES: [u32; 2] = [1, 2];
        vec![
            Rule::Allow(0),
            Rule::ArgIn(1, 0, &VALUES),
            Rule::ArgNoBits(9, 2, 4),
            Rule::ArgsEq(234, vec![(0, 77), (2, 6)]),
            Rule::Errno(39, libc::EPERM),
            Rule::Allow(60),
        ]
    }

    #[test]
    fn the_assembler_follows_its_rules() {
        let f = Filter::new(&rules()).unwrap();
        let go = |nr, args| run(&f, ARCH, nr, args);
        assert_eq!(go(0, [0; 6]), RET_ALLOW);
        assert_eq!(go(60, [0; 6]), RET_ALLOW);
        // Argument lists.
        assert_eq!(go(1, [1, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(1, [2, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(1, [3, 0, 0, 0, 0, 0]), RET_KILL_PROCESS);
        assert_eq!(go(1, [0; 6]), RET_KILL_PROCESS);
        // A mismatch never falls into the next rule: 60 is allowed, but not
        // through the failed block of 1.
        // Bits.
        assert_eq!(go(9, [0, 0, 3, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(9, [0, 0, 4, 0, 0, 0]), RET_KILL_PROCESS);
        assert_eq!(go(9, [0, 0, 5, 0, 0, 0]), RET_KILL_PROCESS);
        // All of several arguments.
        assert_eq!(go(234, [77, 0, 6, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(234, [78, 0, 6, 0, 0, 0]), RET_KILL_PROCESS);
        assert_eq!(go(234, [77, 0, 9, 0, 0, 0]), RET_KILL_PROCESS);
        // Errno.
        assert_eq!(go(39, [0; 6]), RET_ERRNO | libc::EPERM as u32);
        // Anything else, and another architecture, is killed.
        assert_eq!(go(2, [0; 6]), RET_KILL_PROCESS);
        assert_eq!(go(0x4000_0000, [0; 6]), RET_KILL_PROCESS);
        assert_eq!(run(&f, ARCH ^ 1, 0, [0; 6]), RET_KILL_PROCESS);
        assert_eq!(
            run(&f, 0x4000_0003, 1, [1, 0, 0, 0, 0, 0]),
            RET_KILL_PROCESS
        );
    }

    #[test]
    fn an_empty_list_kills_everything() {
        let f = Filter::new(&[]).unwrap();
        for nr in [0, 1, 60, 231] {
            assert_eq!(run(&f, ARCH, nr, [0; 6]), RET_KILL_PROCESS);
        }
    }

    #[test]
    fn a_filter_too_long_for_a_jump_is_refused_not_wrapped() {
        let rules: Vec<Rule> = (0..300).map(Rule::Allow).collect();
        assert!(Filter::new(&rules).is_err());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_inspectors_list_is_small_and_has_no_way_out() {
        let rules = inspector_rules(1234);
        let f = Filter::new(&rules).unwrap();
        assert!(f.len() < 200, "{}", f.len());
        let go = |nr: i64, args| run(&f, ARCH, nr as u32, args);
        for nr in [
            libc::SYS_open,
            libc::SYS_openat,
            libc::SYS_execve,
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_ptrace,
            libc::SYS_clone,
            libc::SYS_fork,
            libc::SYS_unlink,
            libc::SYS_ioctl,
        ] {
            assert_eq!(go(nr, [0; 6]), RET_KILL_PROCESS, "{nr}");
        }
        // Only the standard output and error may be written.
        assert_eq!(go(libc::SYS_write, [1, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(libc::SYS_write, [2, 0, 0, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(libc::SYS_write, [3, 0, 0, 0, 0, 0]), RET_KILL_PROCESS);
        // Signals: itself, SIGABRT.
        assert_eq!(go(libc::SYS_tgkill, [1234, 1234, 6, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(libc::SYS_tgkill, [1, 1, 6, 0, 0, 0]), RET_KILL_PROCESS);
        assert_eq!(
            go(libc::SYS_tgkill, [1234, 1234, 9, 0, 0, 0]),
            RET_KILL_PROCESS
        );
        // Memory is never made executable.
        assert_eq!(go(libc::SYS_mmap, [0, 0, 3, 0, 0, 0]), RET_ALLOW);
        assert_eq!(go(libc::SYS_mmap, [0, 0, 5, 0, 0, 0]), RET_KILL_PROCESS);
        assert_eq!(go(libc::SYS_mprotect, [0, 0, 7, 0, 0, 0]), RET_KILL_PROCESS);
    }
}
