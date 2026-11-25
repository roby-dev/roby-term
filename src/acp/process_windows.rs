use std::{
    io::{self, PipeReader, PipeWriter},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS},
    System::{
        JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
            InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
            CREATE_NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
        },
    },
};

type PipesAndStop = (PipeWriter, PipeReader, PipeReader, Box<dyn FnOnce()>);

pub(super) fn spawn(program: &str, args: &[String], cwd: &Path) -> io::Result<PipesAndStop> {
    let mut command = command_line(program, args)?;
    let mut cwd: Vec<u16> = cwd.as_os_str().encode_wide().collect();
    if cwd.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in cwd"));
    }
    cwd.push(0);

    let job = kill_on_close_job()?;
    let (input, stdin) = io::pipe()?;
    let (stdout, output) = io::pipe()?;
    let (stderr, errors) = io::pipe()?;
    let child_ends = [
        inheritable(input.as_raw_handle())?,
        inheritable(output.as_raw_handle())?,
        inheritable(errors.as_raw_handle())?,
    ];
    let handles = child_ends.each_ref().map(AsRawHandle::as_raw_handle);
    let jobs = [job.as_raw_handle()];
    let attributes = Attributes::new()?;
    // The lists and their owned handles outlive the attribute list and launch.
    // Membership is set by CreateProcess, with no interval for the agent to
    // start children outside the job. Inheritance into the agent is stdio-only.
    unsafe {
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    }
    let info = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: handles[0],
            hStdOutput: handles[1],
            hStdError: handles[2],
            ..Default::default()
        },
        lpAttributeList: attributes.ptr,
    };
    let mut created = PROCESS_INFORMATION::default();
    // Null application name permits Windows to look up a native executable by
    // name. We don't start a shell or interpret shell scripts or arguments.
    check(unsafe {
        CreateProcessW(
            null(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            null(),
            cwd.as_ptr(),
            &info.StartupInfo,
            &mut created,
        )
    })?;
    // A successful CreateProcess transfers these two handles to us.
    let _thread = unsafe { OwnedHandle::from_raw_handle(created.hThread) };
    let child = unsafe { OwnedHandle::from_raw_handle(created.hProcess) };
    let stop = move || {
        drop(job);
        // The job is non-inheritable and this was its last handle. Wait for
        // the direct process to finish terminating, on the reaper thread.
        unsafe { WaitForSingleObject(child.as_raw_handle(), INFINITE) };
    };
    Ok((stdin, stdout, stderr, Box::new(stop)))
}

fn kill_on_close_job() -> io::Result<OwnedHandle> {
    let handle = unsafe { CreateJobObjectW(null(), null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    check(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

fn inheritable(handle: RawHandle) -> io::Result<OwnedHandle> {
    let mut duplicate = null_mut();
    check(unsafe {
        let this = GetCurrentProcess();
        DuplicateHandle(this, handle, this, &mut duplicate, 0, 1, DUPLICATE_SAME_ACCESS)
    })?;
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

struct Attributes {
    ptr: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // The first call with a null list supplies the required allocation size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let ptr = storage.as_mut_ptr().cast();
        check(unsafe { InitializeProcThreadAttributeList(ptr, 2, 0, &mut bytes) })?;
        Ok(Self {
            ptr,
            _storage: storage,
        })
    }

    // The value must stay live until this list has been deleted.
    unsafe fn set<T>(&self, kind: u32, value: &T) -> io::Result<()> {
        check(unsafe {
            UpdateProcThreadAttribute(
                self.ptr,
                0,
                kind as usize,
                (value as *const T).cast(),
                size_of::<T>(),
                null_mut(),
                null(),
            )
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr) };
    }
}

fn check(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn command_line(program: &str, args: &[String]) -> io::Result<Vec<u16>> {
    if program.is_empty() || program.contains(['\0', '"']) || args.iter().any(|a| a.contains('\0')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid ACP command line",
        ));
    }
    let mut line: Vec<u16> = format!("\"{program}\"").encode_utf16().collect();
    for arg in args {
        line.extend([b' ' as u16, b'"' as u16]);
        let mut slashes = 0;
        // The Windows argv parser treats backslashes literally except before a
        // quote, where they escape both backslashes and the quote itself.
        for code in arg.encode_utf16() {
            match code {
                92 => slashes += 1,
                _ => {
                    let count = if code == 34 { 2 * slashes + 1 } else { slashes };
                    line.extend(std::iter::repeat_n(92, count));
                    line.push(code);
                    slashes = 0;
                }
            }
        }
        line.extend(std::iter::repeat_n(92, 2 * slashes));
        line.push(b'"' as u16);
    }
    line.push(0);
    Ok(line)
}
