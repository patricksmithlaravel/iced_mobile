//! iOS: the version of the SDK the executable was linked with.
//!
//! The linker writes it into the executable's Mach-O header, in the `sdk`
//! field of `LC_BUILD_VERSION` (or of `LC_VERSION_MIN_IPHONEOS` for old
//! deployment targets). UIKit reads the same field to decide which behaviour
//! an app gets, such as whether it must adopt the scene life cycle.

// dyld's header is reached through a C call and raw reads; each says why it
// is sound.
#![allow(unsafe_code)]

/// The SDK version of the running executable, as `(major, minor)`.
#[cfg(any(target_os = "ios", all(test, target_os = "macos")))]
pub(crate) fn linked() -> Option<(u32, u32)> {
    // `<mach-o/dyld.h>`; image 0 is the main executable.
    unsafe extern "C" {
        fn _dyld_get_image_header(image_index: u32) -> *const u8;
    }

    // SAFETY: dyld returns the header it mapped for the main executable,
    // which stays mapped for the life of the process, or null.
    let header = unsafe { _dyld_get_image_header(0) };

    if header.is_null() {
        return None;
    }

    // SAFETY: a mapped Mach-O header is at least `HEADER` bytes long.
    let fixed = unsafe { std::slice::from_raw_parts(header, HEADER) };

    if read(fixed, 0)? != MH_MAGIC_64 {
        return None;
    }

    let size = HEADER + usize::try_from(read(fixed, 20)?).ok()?;

    // SAFETY: the load commands follow the header in the same mapping, and
    // `sizeofcmds` is their total size.
    let header = unsafe { std::slice::from_raw_parts(header, size) };

    sdk_version(header)
}

/// `MH_MAGIC_64`, in the byte order of the machine.
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// The size of `mach_header_64`.
const HEADER: usize = 32;
const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
const LC_BUILD_VERSION: u32 = 0x32;

/// The SDK version a 64-bit Mach-O header and its load commands record.
fn sdk_version(header: &[u8]) -> Option<(u32, u32)> {
    if read(header, 0)? != MH_MAGIC_64 {
        return None;
    }

    let count = read(header, 16)?;
    let mut offset = HEADER;

    for _ in 0..count {
        let command = read(header, offset)?;
        let size = usize::try_from(read(header, offset + 4)?).ok()?;

        // `build_version_command`: cmd, cmdsize, platform, minos, sdk.
        // `version_min_command`: cmd, cmdsize, version, sdk.
        let sdk = match command {
            LC_BUILD_VERSION => Some(read(header, offset + 16)?),
            LC_VERSION_MIN_IPHONEOS => Some(read(header, offset + 12)?),
            _ => None,
        };

        // Encoded as xxxx.yy.zz in nibbles.
        if let Some(sdk) = sdk {
            return Some((sdk >> 16, (sdk >> 8) & 0xff));
        }

        if size < 8 {
            return None;
        }

        offset = offset.checked_add(size)?;
    }

    None
}

fn read(bytes: &[u8], offset: usize) -> Option<u32> {
    let bytes = bytes.get(offset..offset.checked_add(4)?)?;

    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(commands: &[&[u32]]) -> Vec<u8> {
        let words: Vec<u32> =
            commands.iter().flat_map(|c| c.iter()).copied().collect();
        let size = u32::try_from(words.len() * 4).unwrap();
        let count = u32::try_from(commands.len()).unwrap();

        [MH_MAGIC_64, 0x0100_000c, 0, 2, count, size, 0, 0]
            .iter()
            .chain(&words)
            .flat_map(|word| word.to_ne_bytes())
            .collect()
    }

    #[test]
    fn reads_the_sdk_of_a_build_version_command() {
        // LC_SEGMENT_64 stand-in, then LC_BUILD_VERSION for the iOS
        // simulator (platform 7), minimum 16.0, SDK 27.0, no tools.
        let header = header(&[
            &[0x19, 16, 0, 0],
            &[LC_BUILD_VERSION, 24, 7, 0x0010_0000, 0x001b_0000, 0],
        ]);

        assert_eq!(sdk_version(&header), Some((27, 0)));
    }

    #[test]
    fn reads_the_sdk_of_a_version_min_command() {
        let header =
            header(&[&[LC_VERSION_MIN_IPHONEOS, 16, 0x000a_0000, 0x0012_0400]]);

        assert_eq!(sdk_version(&header), Some((18, 4)));
    }

    #[test]
    fn refuses_what_is_not_a_complete_64_bit_header() {
        let mut wrong_magic = header(&[&[LC_BUILD_VERSION, 24, 2, 0, 0, 0]]);
        wrong_magic[..4].copy_from_slice(&0xfeed_face_u32.to_ne_bytes());

        let mut truncated = header(&[&[LC_BUILD_VERSION, 24, 2, 0, 0, 0]]);
        truncated.truncate(HEADER + 12);

        let zero_size =
            header(&[&[0x19, 0], &[LC_BUILD_VERSION, 24, 2, 0, 0, 0]]);

        assert_eq!(sdk_version(&wrong_magic), None);
        assert_eq!(sdk_version(&truncated), None);
        assert_eq!(sdk_version(&zero_size), None);
        assert_eq!(sdk_version(&header(&[])), None);
    }

    /// The test binary's own header, as dyld mapped it: macOS executables
    /// record their SDK the same way.
    #[cfg(target_os = "macos")]
    #[test]
    fn reads_the_sdk_of_the_running_executable() {
        let (major, _minor) = linked().expect("SDK version of the test binary");

        assert!(major >= 10, "SDK major version {major}");
    }
}
