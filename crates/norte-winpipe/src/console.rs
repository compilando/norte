//! The console's code page (ADR 0162).
//!
//! norte writes UTF-8; a Windows console reads output in its code page,
//! which is the OEM one (437, 850…) unless the system opted into UTF-8. So
//! every box-drawing character came out as three and `ntc`'s screen fell
//! apart. The page is switched for the run and put back afterwards.

use windows_sys::Win32::Globalization::CP_UTF8;
use windows_sys::Win32::System::Console::{
    GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP,
};

/// The console's pages before [`console_utf8`]; restored when dropped.
#[derive(Debug)]
pub struct ConsoleUtf8 {
    input: u32,
    output: u32,
}

/// Switches the attached console to UTF-8, input and output.
///
/// `None` if there is no console (a service, output redirected to a file
/// with no console at all): nothing was changed and nothing needs restoring.
#[allow(unsafe_code)]
#[must_use]
pub fn console_utf8() -> Option<ConsoleUtf8> {
    // SAFETY: the four calls take and return plain integers; with no
    // console the getters answer 0, which is checked before anything is
    // set.
    unsafe {
        let (input, output) = (GetConsoleCP(), GetConsoleOutputCP());
        if input == 0 || output == 0 {
            return None;
        }
        SetConsoleOutputCP(CP_UTF8);
        SetConsoleCP(CP_UTF8);
        Some(ConsoleUtf8 { input, output })
    }
}

/// The console's output code page, or `None` without a console.
#[allow(unsafe_code)]
#[must_use]
pub fn output_code_page() -> Option<u32> {
    // SAFETY: takes nothing, returns an integer; 0 means no console.
    let page = unsafe { GetConsoleOutputCP() };
    (page != 0).then_some(page)
}

impl Drop for ConsoleUtf8 {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: plain integers, pages this process read from the same
        // console at start.
        unsafe {
            SetConsoleOutputCP(self.output);
            SetConsoleCP(self.input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTF-8 while held, the previous page after: the reader's console is
    /// handed back as it was.
    #[test]
    fn the_page_is_switched_and_put_back() {
        let Some(before) = output_code_page() else {
            // No console (a detached CI runner): nothing to switch.
            return;
        };
        {
            let _utf8 = console_utf8().expect("there is a console");
            assert_eq!(output_code_page(), Some(CP_UTF8));
        }
        assert_eq!(output_code_page(), Some(before));
    }
}
