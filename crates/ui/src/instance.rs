//! One app per install: a named mutex that exists while an app of the install runs, and a named
//! auto-reset event through which a second start asks that app to show its window.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

/// How often a claim that may wait tries the mutex again.
const RETRY_EVERY: Duration = Duration::from_millis(100);

/// Local\StreamLoop-app-H and Local\StreamLoop-show-H, H the first 16 hex digits of SHA-256 of the root's
/// full path, lowercased; "nil" for H without a root.
pub fn instance_names(root: Option<&Path>) -> (String, String) {
    let h = match root {
        Some(root) => {
            let digest = Sha256::digest(root.to_string_lossy().to_lowercase().as_bytes());
            digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
        }
        None => "nil".to_string(),
    };
    (
        format!(r"Local\StreamLoop-app-{h}"),
        format!(r"Local\StreamLoop-show-{h}"),
    )
}

/// This app's hold on its install. The lock goes when it is dropped or the process ends.
pub struct Instance {
    mutex: sys::Handle,
    /// Held from the claim on, so a show signal sent before `on_show` waits in the event.
    show: sys::Handle,
    show_name: String,
}

pub enum Claim {
    Ours(Instance),
    Running,
}

/// The install's lock, or `Running` when another app of it holds the lock. With a `wait`, tries again
/// until the other app lets go or the wait ends.
pub fn claim(root: Option<&Path>, wait: Duration) -> Claim {
    let (mutex_name, show_name) = instance_names(root);
    let until = Instant::now() + wait;
    loop {
        if let Some(mutex) = sys::new_mutex(&mutex_name) {
            let show = sys::event(&show_name);
            return Claim::Ours(Instance {
                mutex,
                show,
                show_name,
            });
        }
        let now = Instant::now();
        if now >= until {
            return Claim::Running;
        }
        std::thread::sleep(RETRY_EVERY.min(until - now));
    }
}

/// Asks the app that holds the lock to show its window, and lets it take the foreground.
pub fn signal_show(root: Option<&Path>) {
    sys::signal(&instance_names(root).1);
}

impl Instance {
    /// Calls `f` on a thread of its own each time a second start asks for the window.
    pub fn on_show(&self, f: impl Fn() + Send + 'static) {
        // The thread waits on a handle of its own, so dropping the Instance never closes one under it.
        let Some(event) = sys::open_event(&self.show_name) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("show-signal".into())
            .spawn(move || {
                while sys::wait(&event) {
                    f();
                }
            });
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        sys::close(&self.show);
        sys::close(&self.mutex);
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject},
        UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow},
    };

    /// A kernel handle as an address, so it can cross to another thread; 0 when a call failed.
    pub struct Handle(usize);

    impl Handle {
        fn raw(&self) -> HANDLE {
            self.0 as HANDLE
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A mutex of that name made by this call; None when one already exists. A failed call counts as
    /// made, so the app still opens its window.
    pub fn new_mutex(name: &str) -> Option<Handle> {
        let name = wide(name);
        // SAFETY: the name is NUL-terminated and outlives the call; the error is read right after it.
        unsafe {
            let mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(mutex);
                return None;
            }
            Some(Handle(mutex as usize))
        }
    }

    /// The auto-reset event of that name, made unsignalled when it does not exist yet.
    pub fn event(name: &str) -> Handle {
        let name = wide(name);
        // SAFETY: the name is NUL-terminated and outlives the call.
        Handle(unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) } as usize)
    }

    pub fn open_event(name: &str) -> Option<Handle> {
        Some(event(name)).filter(|h| h.0 != 0)
    }

    /// False once the wait fails, so a broken handle never spins.
    pub fn wait(event: &Handle) -> bool {
        // SAFETY: the handle stays open for as long as the thread waiting on it runs.
        unsafe { WaitForSingleObject(event.raw(), INFINITE) == WAIT_OBJECT_0 }
    }

    pub fn signal(name: &str) {
        // SAFETY: no pointers; it only lets any process take the foreground with this start's rights.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        let event = event(name);
        if event.0 != 0 {
            // SAFETY: the handle was just opened and is closed after the signal.
            unsafe { SetEvent(event.raw()) };
            close(&event);
        }
    }

    pub fn close(handle: &Handle) {
        if handle.0 != 0 {
            // SAFETY: the handle is this process's own and is closed once, by its owner's drop.
            unsafe { CloseHandle(handle.raw()) };
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub struct Handle;

    pub fn new_mutex(_name: &str) -> Option<Handle> {
        Some(Handle)
    }

    pub fn event(_name: &str) -> Handle {
        Handle
    }

    pub fn open_event(_name: &str) -> Option<Handle> {
        None
    }

    pub fn wait(_event: &Handle) -> bool {
        false
    }

    pub fn signal(_name: &str) {}

    pub fn close(_handle: &Handle) {}
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::*;

    /// A root of this test process alone, so parallel tests never share a lock.
    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sl-ui-instance-{name}-{}", std::process::id()))
    }

    #[test]
    fn names_ignore_case_and_differ_per_install() {
        // Arrange
        let one = Path::new(r"C:\A\sl4 Ö (x)");
        let same = Path::new(r"c:\a\SL4 ö (X)");
        let other = Path::new(r"C:\B\sl4");

        // Act
        let (a, b, c) = (
            instance_names(Some(one)),
            instance_names(Some(same)),
            instance_names(Some(other)),
        );

        // Assert
        assert_eq!(a, b);
        assert_ne!(a.0, c.0);
        assert_ne!(a.1, c.1);
        let hash = a.0.strip_prefix(r"Local\StreamLoop-app-").expect("the mutex name");
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{hash}");
        assert_eq!(a.1, format!(r"Local\StreamLoop-show-{hash}"));
    }

    #[test]
    fn names_without_a_root_end_in_nil() {
        // Act
        let names = instance_names(None);

        // Assert
        assert_eq!(
            names,
            (r"Local\StreamLoop-app-nil".to_string(), r"Local\StreamLoop-show-nil".to_string())
        );
    }

    #[test]
    fn a_second_claim_sees_the_first_running() {
        // Arrange
        let root = root("second");
        let first = claim(Some(&root), Duration::ZERO);

        // Act
        let second = claim(Some(&root), Duration::ZERO);
        let elsewhere = claim(Some(&root.join("other")), Duration::ZERO);

        // Assert
        assert!(matches!(first, Claim::Ours(_)));
        assert!(matches!(second, Claim::Running));
        assert!(matches!(elsewhere, Claim::Ours(_)));
    }

    #[test]
    fn a_claim_that_waits_takes_the_lock_once_the_first_lets_go() {
        // Arrange
        let root = root("wait");
        let Claim::Ours(first) = claim(Some(&root), Duration::ZERO) else {
            panic!("the first claim is ours");
        };
        let held = Instant::now();
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });

        // Act
        let short = claim(Some(&root), Duration::from_millis(100));
        let long = claim(Some(&root), Duration::from_secs(10));

        // Assert
        letting_go.join().unwrap();
        assert!(matches!(short, Claim::Running));
        assert!(matches!(long, Claim::Ours(_)));
        assert!(held.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn the_show_signal_reaches_the_first_instance() {
        // Arrange
        let root = root("show");
        let Claim::Ours(first) = claim(Some(&root), Duration::ZERO) else {
            panic!("the first claim is ours");
        };
        let (tx, rx) = mpsc::channel();
        first.on_show(move || {
            let _ = tx.send(());
        });

        // Act
        signal_show(Some(&root));

        // Assert
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(()));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "one signal, one show");
    }
}
