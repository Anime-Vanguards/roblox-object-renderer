//! The account signed in to Roblox Studio on this machine: its `.ROBLOSECURITY` cookie, read from where Studio
//! keeps it. The account's last source, after the environment and the env files (the `cookie` module), and only
//! when env `ROR_STUDIO_COOKIE` is `1` (any other value is off).
//!
//! - Windows: the Credential Manager's generic credentials `https://www.roblox.com:RobloxStudioAuthuserid` (the
//!   signed-in account's user id) and `https://www.roblox.com:RobloxStudioAuth.ROBLOSECURITY<user id>` (its
//!   cookie). Without a user id: `...RobloxStudioAuth.ROBLOSECURITY` alone, else the one account with a cookie
//!   there. Older Studio builds kept it in the registry instead (`HKCU\Software\Roblox\RobloxStudioBrowser\roblox.com`,
//!   value `.ROBLOSECURITY`: `SEC::<...>,EXP::<...>,COOK::<cookie>`), read when the Credential Manager has none.
//! - macOS: `~/Library/HTTPStorages/com.Roblox.RobloxStudio.binarycookies`, Apple's binary cookie file, where
//!   Studio keeps its sign-in as cookies of a `roblox.com` domain under the names its Windows credentials end
//!   with: `/RobloxStudioAuth/userid` (the signed-in account's user id) and
//!   `/RobloxStudioAuth/.ROBLOSECURITY<user id>` (its cookie), read as the Credential Manager's are. With none
//!   of those, a cookie named `.ROBLOSECURITY` itself.
//! - Elsewhere: not available.
//!
//! The OS calls only hand over the stored bytes; the parsing here is tested on synthetic data. The cookie is
//! held in [`Secret`] like the others, and the bytes it was read from are zeroed. A reason why it could not be
//! read names no value, user id or credential name.

// the Windows readers are tested everywhere, used on Windows alone
#![cfg_attr(not(windows), allow(dead_code))]

use crate::cookie::Secret;

/// The switch: `1` reads Studio's account, anything else does not.
pub(crate) const SWITCH: &str = "ROR_STUDIO_COOKIE";

/// The Credential Manager's target names Studio's start with: `<TARGET>userid` holds the signed-in account's user
/// id, `<TARGET>.ROBLOSECURITY<user id>` its cookie.
const TARGET: &str = "https://www.roblox.com:RobloxStudioAuth";

/// Where the cookie was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Store {
	CredentialManager,
	Registry,
	/// macOS only
	#[cfg_attr(windows, allow(dead_code))]
	BinaryCookies,
}

impl Store {
	pub(crate) fn label(self) -> &'static str {
		match self {
			Store::CredentialManager => "Windows Credential Manager",
			Store::Registry => "Windows registry",
			Store::BinaryCookies => "macOS cookie file",
		}
	}
}

/// What looking for Studio's cookie found.
#[derive(Debug)]
pub(crate) enum Lookup {
	/// `ROR_STUDIO_COOKIE` is not `1`
	Off,
	/// No reader on this platform (neither Windows nor macOS)
	#[cfg_attr(windows, allow(dead_code))]
	Unsupported,
	NotFound,
	Found(Secret, Store),
	/// Why it could not be read (never a value)
	Unreadable(String),
}

impl Lookup {
	/// `off (set ROR_STUDIO_COOKIE=1)`, `not found`, `found (<store>)`, `unreadable: <reason>`: never a value.
	pub(crate) fn status(&self) -> String {
		match self {
			Lookup::Off => format!("off (set {SWITCH}=1)"),
			Lookup::Unsupported => "not available on this platform".into(),
			Lookup::NotFound => "not found".into(),
			Lookup::Found(_, store) => format!("found ({})", store.label()),
			Lookup::Unreadable(why) => format!("unreadable: {why}"),
		}
	}

	pub(crate) fn into_found(self) -> Option<(Secret, Store)> {
		match self {
			Lookup::Found(secret, store) => Some((secret, store)),
			_ => None,
		}
	}
}

/// Whether the switch's value turns the source on: exactly `1`.
pub(crate) fn enabled(value: Option<&std::ffi::OsStr>) -> bool {
	value.is_some_and(|value| value == "1")
}

/// Studio's cookie when this process's `ROR_STUDIO_COOKIE` is `1` (the store is not touched otherwise).
pub(crate) fn load() -> Lookup {
	if !enabled(std::env::var_os(SWITCH).as_deref()) {
		return Lookup::Off;
	}
	read()
}

/// The line `cookie_diagnostics` gives the source: `studio account: <status>` ([`Lookup::status`]).
pub(crate) fn diagnose() -> String {
	format!("studio account: {}", load().status())
}

/// Bytes that may hold the cookie: zeroed on drop, like [`Secret`].
struct Blob(Vec<u8>);

impl Drop for Blob {
	fn drop(&mut self) {
		for byte in self.0.iter_mut() {
			// SAFETY: `byte` is a valid, aligned, exclusive reference into the vector
			unsafe { std::ptr::write_volatile(byte, 0) };
		}
	}
}

/// A stored value as text bytes: UTF-8 as it is, UTF-16 (little-endian, as Windows writes wide strings) when
/// every second byte is 0; trailing NULs dropped.
fn decoded(stored: &[u8]) -> Blob {
	let wide = stored.len() >= 2 && stored.len().is_multiple_of(2) && stored.iter().skip(1).step_by(2).all(|&byte| byte == 0);
	let mut bytes: Vec<u8> = if wide { stored.iter().step_by(2).copied().collect() } else { stored.to_vec() };
	while bytes.last() == Some(&0) {
		bytes.pop();
	}
	Blob(bytes)
}

/// The cookie in a stored value, when it is one ([`Secret::new`]'s check; a `.ROBLOSECURITY=` prefix is dropped).
fn cookie_of(stored: &[u8]) -> Option<Secret> {
	let text = decoded(stored);
	std::str::from_utf8(&text.0).ok().and_then(Secret::new)
}

/// A found value: the cookie, or why the store's entry is not one.
fn usable(cookie: Option<Secret>, store: Store) -> Lookup {
	match cookie {
		Some(secret) => Lookup::Found(secret, store),
		None => Lookup::Unreadable(format!("the {} entry holds no usable cookie", store.label())),
	}
}

fn digits(text: &str) -> bool {
	!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The cookie in the Credential Manager. `read` gives a generic credential's stored bytes by its name (None when
/// there is none), `names` the names that match a `<prefix>*` filter; their errors name no value.
///
/// The signed-in account's cookie (its user id in `<TARGET>userid`), else the one without a user id that builds
/// with a single account kept. Only with no user id at all, the cookie of the one account that has one: with
/// several, which is signed in is unknown.
fn from_credentials(read: &dyn Fn(&str) -> Result<Option<Blob>, String>, names: &dyn Fn(&str) -> Result<Vec<String>, String>) -> Lookup {
	let prefix = format!("{TARGET}.ROBLOSECURITY");
	let user = match read(&format!("{TARGET}userid")) {
		Ok(stored) => stored.and_then(|stored| std::str::from_utf8(&decoded(&stored.0).0).ok().map(|text| text.trim().to_string())).filter(|id| digits(id)),
		Err(why) => return Lookup::Unreadable(why),
	};
	let mut candidates: Vec<String> = user.iter().map(|id| format!("{prefix}{id}")).collect();
	candidates.push(prefix.clone());
	for name in &candidates {
		match read(name) {
			Ok(Some(stored)) => return usable(cookie_of(&stored.0), Store::CredentialManager),
			Ok(None) => {}
			Err(why) => return Lookup::Unreadable(why),
		}
	}
	if user.is_some() {
		return Lookup::NotFound;
	}
	let accounts: Vec<String> = match names(&format!("{prefix}*")) {
		Ok(names) => names.into_iter().filter(|name| name.strip_prefix(&prefix).is_some_and(digits)).collect(),
		Err(why) => return Lookup::Unreadable(why),
	};
	match accounts.as_slice() {
		[] => Lookup::NotFound,
		[name] => match read(name) {
			Ok(Some(stored)) => usable(cookie_of(&stored.0), Store::CredentialManager),
			Ok(None) => Lookup::NotFound,
			Err(why) => Lookup::Unreadable(why),
		},
		_ => Lookup::Unreadable("several Studio accounts have a cookie and none is marked as signed in".into()),
	}
}

/// The cookie in the registry value older Studio builds wrote: comma-separated `KEY::<value>` items, the cookie
/// in `COOK::<...>`.
fn from_registry_value(value: &str) -> Lookup {
	let cookie = value.split(',').find_map(|item| item.trim().strip_prefix("COOK::")).map(|item| item.strip_prefix('<').and_then(|item| item.strip_suffix('>')).unwrap_or(item));
	match cookie {
		Some(cookie) => usable(Secret::new(cookie), Store::Registry),
		None => Lookup::Unreadable(format!("the {} entry holds no cookie", Store::Registry.label())),
	}
}

#[cfg(windows)]
fn read() -> Lookup {
	match from_credentials(&os::credential, &os::credential_names) {
		Lookup::NotFound => match os::registry_value() {
			Ok(Some(value)) => match std::str::from_utf8(&value.0) {
				Ok(text) => from_registry_value(text),
				Err(_) => Lookup::Unreadable(format!("the {} entry is not text", Store::Registry.label())),
			},
			Ok(None) => Lookup::NotFound,
			Err(why) => Lookup::Unreadable(why),
		},
		other => other,
	}
}

#[cfg(target_os = "macos")]
fn read() -> Lookup {
	let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
		return Lookup::Unreadable("HOME is not set".into());
	};
	let file = std::path::PathBuf::from(home).join("Library").join("HTTPStorages").join("com.Roblox.RobloxStudio.binarycookies");
	match std::fs::read(&file) {
		Ok(bytes) => {
			let bytes = Blob(bytes);
			match parse_binary_cookies(&bytes.0) {
				Ok(Some(secret)) => Lookup::Found(secret, Store::BinaryCookies),
				Ok(None) => Lookup::NotFound,
				Err(why) => Lookup::Unreadable(why.into()),
			}
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lookup::NotFound,
		Err(error) => Lookup::Unreadable(format!("the {}: {}", Store::BinaryCookies.label(), error.kind())),
	}
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read() -> Lookup {
	Lookup::Unsupported
}

/// The Credential Manager and the registry through windows-sys: the stored bytes only, read here and parsed above.
#[cfg(windows)]
mod os {
	use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, GetLastError};
	use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredEnumerateW, CredFree, CredReadW};
	use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

	use super::Blob;

	fn wide(text: &str) -> Vec<u16> {
		text.encode_utf16().chain(std::iter::once(0)).collect()
	}

	/// Copies a credential's stored bytes out and zeroes Windows' copy, which the caller then frees.
	///
	/// # Safety
	///
	/// `credential` comes from CredReadW or CredEnumerateW and has not been freed.
	unsafe fn take_blob(credential: &CREDENTIALW) -> Vec<u8> {
		let (blob, size) = (credential.CredentialBlob, credential.CredentialBlobSize as usize);
		if blob.is_null() || size == 0 {
			return Vec::new();
		}
		// SAFETY: the API's credential holds `size` bytes at `blob`, in memory that is the caller's until freed
		unsafe {
			let bytes = std::slice::from_raw_parts(blob, size).to_vec();
			std::ptr::write_bytes(blob, 0, size);
			bytes
		}
	}

	/// A generic credential's stored bytes; None when there is none of that name.
	pub(super) fn credential(target: &str) -> Result<Option<Blob>, String> {
		let name = wide(target);
		let mut found: *mut CREDENTIALW = std::ptr::null_mut();
		// SAFETY: `name` is NUL-terminated; on success `found` points at one credential, freed below
		if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) } == 0 {
			// SAFETY: no precondition
			let code = unsafe { GetLastError() };
			return if code == ERROR_NOT_FOUND { Ok(None) } else { Err(format!("Windows Credential Manager error {code}")) };
		}
		// SAFETY: CredReadW succeeded, so `found` is valid until CredFree
		let bytes = unsafe { take_blob(&*found) };
		// SAFETY: allocated by CredReadW, freed once
		unsafe { CredFree(found.cast()) };
		Ok(Some(Blob(bytes)))
	}

	/// The names of the generic credentials that match `filter` (`<prefix>*`).
	pub(super) fn credential_names(filter: &str) -> Result<Vec<String>, String> {
		let filter = wide(filter);
		let (mut count, mut list): (u32, *mut *mut CREDENTIALW) = (0, std::ptr::null_mut());
		// SAFETY: `filter` is NUL-terminated; on success `list` holds `count` credentials, freed below
		if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut list) } == 0 {
			// SAFETY: no precondition
			let code = unsafe { GetLastError() };
			return if code == ERROR_NOT_FOUND { Ok(Vec::new()) } else { Err(format!("Windows Credential Manager error {code}")) };
		}
		let mut names = Vec::new();
		for index in 0..count as usize {
			// SAFETY: `list` holds `count` valid credential pointers until CredFree
			let credential = unsafe { &**list.add(index) };
			// every credential the filter matched came with its stored bytes: zero them before they are freed
			// SAFETY: as above
			drop(Blob(unsafe { take_blob(credential) }));
			if credential.Type != CRED_TYPE_GENERIC || credential.TargetName.is_null() {
				continue;
			}
			// SAFETY: TargetName is a NUL-terminated wide string of the credential
			let name = unsafe {
				let length = (0..).take_while(|&at| *credential.TargetName.add(at) != 0).count();
				String::from_utf16_lossy(std::slice::from_raw_parts(credential.TargetName, length))
			};
			names.push(name);
		}
		// SAFETY: allocated by CredEnumerateW, freed once
		unsafe { CredFree(list.cast()) };
		Ok(names)
	}

	/// The registry value older Studio builds kept the cookie in, as UTF-8; None when it is not there.
	pub(super) fn registry_value() -> Result<Option<Blob>, String> {
		let key = wide("Software\\Roblox\\RobloxStudioBrowser\\roblox.com");
		let value = wide(".ROBLOSECURITY");
		let mut size: u32 = 0;
		// SAFETY: valid NUL-terminated wide strings; a null data pointer asks for the size
		let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
		if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
			return Ok(None);
		}
		if status != ERROR_SUCCESS {
			return Err(format!("Windows registry error {status}"));
		}
		if size > 64 * 1024 {
			return Err("the Windows registry entry is too long".into());
		}
		let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
		let mut size = (buffer.len() * 2) as u32;
		// SAFETY: `buffer` holds `size` bytes
		let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size) };
		let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
		let text = (status == ERROR_SUCCESS).then(|| String::from_utf16(&buffer[..length]).ok()).flatten();
		for unit in buffer.iter_mut() {
			// SAFETY: a valid, aligned, exclusive reference into the buffer
			unsafe { std::ptr::write_volatile(unit, 0) };
		}
		match (status, text) {
			(ERROR_SUCCESS, Some(text)) => Ok(Some(Blob(text.into_bytes()))),
			(ERROR_SUCCESS, None) => Err("the Windows registry entry is not text".into()),
			(status, _) => Err(format!("Windows registry error {status}")),
		}
	}
}

/// The `.ROBLOSECURITY` cookie of a `roblox.com` domain in Apple's binary cookie file, the one that expires last
/// when there are several; None when the file holds none. An error says what is malformed, never a value.
///
/// The format: `cook`, the page count and each page's size (big-endian u32s), then the pages. A page: the bytes
/// `00 00 01 00`, its cookie count and each cookie's offset in the page (little-endian u32s from here on). A
/// cookie: its size, at 16, 20 and 28 the offsets in the cookie of its domain, name and value (NUL-terminated
/// strings), at 40 its expiry (an f64, seconds since 2001).
#[cfg(any(target_os = "macos", test))]
fn parse_binary_cookies(file: &[u8]) -> Result<Option<Secret>, &'static str> {
	const TRUNCATED: &str = "the macOS cookie file is cut short or malformed";
	fn word(bytes: &[u8], at: usize, big_endian: bool) -> Result<usize, &'static str> {
		let word: [u8; 4] = bytes.get(at..at.checked_add(4).ok_or(TRUNCATED)?).and_then(|word| word.try_into().ok()).ok_or(TRUNCATED)?;
		Ok(if big_endian { u32::from_be_bytes(word) } else { u32::from_le_bytes(word) } as usize)
	}
	fn string(cookie: &[u8], at: usize) -> Result<&[u8], &'static str> {
		let rest = cookie.get(at..).ok_or(TRUNCATED)?;
		rest.iter().position(|&byte| byte == 0).map(|end| &rest[..end]).ok_or(TRUNCATED)
	}
	fn roblox(domain: &[u8]) -> bool {
		let domain = domain.strip_prefix(b".").unwrap_or(domain);
		const SUFFIX: &[u8] = b".roblox.com";
		domain.eq_ignore_ascii_case(&SUFFIX[1..]) || (domain.len() > SUFFIX.len() && domain[domain.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX))
	}

	/// What Studio's own entries are named by: `<AUTH>userid`, `<AUTH>.ROBLOSECURITY<user id>` (the ends of its
	/// Windows credentials' names, [`TARGET`])
	const AUTH: &[u8] = b"/RobloxStudioAuth/";
	fn digits(text: &[u8]) -> bool {
		!text.is_empty() && text.iter().all(u8::is_ascii_digit)
	}

	if file.get(..4) != Some(b"cook".as_slice()) {
		return Err("the macOS cookie file is not a binary cookie file");
	}
	let pages = word(file, 4, true)?;
	let mut start = pages.checked_mul(4).and_then(|sizes| sizes.checked_add(8)).filter(|&start| start <= file.len()).ok_or(TRUNCATED)?;
	// a cookie named `.ROBLOSECURITY` itself: the one that expires last
	let mut best: Option<(f64, &[u8])> = None;
	// Studio's own entries: the signed-in account's user id, and each account's cookie by its user id (none: the
	// one a build with a single account keeps)
	let mut user: Option<&[u8]> = None;
	let mut accounts: Vec<(&[u8], &[u8])> = Vec::new();
	for index in 0..pages {
		let size = word(file, 8 + index * 4, true)?;
		let page = file.get(start..start.checked_add(size).ok_or(TRUNCATED)?).ok_or(TRUNCATED)?;
		start += size;
		if page.get(..4) != Some([0u8, 0, 1, 0].as_slice()) {
			return Err(TRUNCATED);
		}
		let count = word(page, 4, false)?;
		if count.checked_mul(4).and_then(|offsets| offsets.checked_add(8)).is_none_or(|end| end > page.len()) {
			return Err(TRUNCATED);
		}
		for at in 0..count {
			let offset = word(page, 8 + at * 4, false)?;
			let length = word(page, offset, false)?;
			let cookie = page.get(offset..offset.checked_add(length).ok_or(TRUNCATED)?).filter(|cookie| cookie.len() >= 48).ok_or(TRUNCATED)?;
			let (domain, name, value) = (string(cookie, word(cookie, 16, false)?)?, string(cookie, word(cookie, 20, false)?)?, string(cookie, word(cookie, 28, false)?)?);
			let expiry = f64::from_le_bytes(cookie[40..48].try_into().map_err(|_| TRUNCATED)?);
			if !roblox(domain) {
				continue;
			}
			if name == b".ROBLOSECURITY" {
				if best.is_none_or(|(latest, _)| expiry > latest) {
					best = Some((expiry, value));
				}
			} else if let Some(entry) = name.strip_prefix(AUTH) {
				if entry == b"userid" {
					user = Some(value);
				} else if let Some(id) = entry.strip_prefix(b".ROBLOSECURITY").filter(|id| id.is_empty() || digits(id)) {
					accounts.push((id, value));
				}
			}
		}
	}
	// Studio's own entries, as its Windows store's are read (from_credentials): the signed-in account's cookie,
	// else the one kept without a user id; with no user id at all, the cookie of the one account that has one
	let of = |id: &[u8]| accounts.iter().find(|(account, _)| *account == id).map(|&(_, value)| value);
	let studio = match user.filter(|id| digits(id)) {
		Some(id) => of(id).or_else(|| of(b"")),
		None => match (of(b""), accounts.as_slice()) {
			(Some(value), _) => Some(value),
			(None, []) => None,
			(None, [(_, value)]) => Some(*value),
			(None, _) => return Err("several Studio accounts have a cookie in the macOS cookie file and none is marked as signed in"),
		},
	};
	match studio.or(best.map(|(_, value)| value)) {
		None => Ok(None),
		Some(value) => std::str::from_utf8(value).ok().and_then(Secret::new).map(Some).ok_or("the macOS cookie file's .ROBLOSECURITY cookie holds no usable value"),
	}
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	const USER: &str = "12345";
	const COOKIE: &str = "TEST-STUDIO-COOKIE-0123456789";

	/// A fake Credential Manager over `entries` (target name, stored bytes).
	fn lookup(entries: &[(String, Vec<u8>)]) -> Lookup {
		let store: HashMap<String, Vec<u8>> = entries.iter().cloned().collect();
		let read = |name: &str| -> Result<Option<Blob>, String> { Ok(store.get(name).map(|bytes| Blob(bytes.clone()))) };
		let names = |filter: &str| -> Result<Vec<String>, String> { Ok(store.keys().filter(|name| name.starts_with(filter.trim_end_matches('*'))).cloned().collect()) };
		from_credentials(&read, &names)
	}

	fn utf16(text: &str) -> Vec<u8> {
		text.encode_utf16().flat_map(u16::to_le_bytes).collect()
	}

	fn found_value(lookup: Lookup) -> String {
		match lookup {
			Lookup::Found(secret, Store::CredentialManager) => secret.expose().to_string(),
			other => panic!("expected a cookie from the Credential Manager, got {}", other.status()),
		}
	}

	#[test]
	fn only_one_switches_it_on() {
		assert!(enabled(Some(std::ffi::OsStr::new("1"))));
		for off in ["0", "true", "yes", " 1", "1 ", ""] {
			assert!(!enabled(Some(std::ffi::OsStr::new(off))), "{off:?}");
		}
		assert!(!enabled(None));
	}

	#[test]
	fn statuses_never_hold_a_value() {
		assert_eq!(Lookup::Off.status(), "off (set ROR_STUDIO_COOKIE=1)");
		assert_eq!(Lookup::NotFound.status(), "not found");
		assert_eq!(Lookup::Unsupported.status(), "not available on this platform");
		assert_eq!(Lookup::Found(Secret::new(COOKIE).unwrap(), Store::CredentialManager).status(), "found (Windows Credential Manager)");
		assert_eq!(Lookup::Found(Secret::new(COOKIE).unwrap(), Store::BinaryCookies).status(), "found (macOS cookie file)");
		let found = Lookup::Found(Secret::new(COOKIE).unwrap(), Store::Registry);
		assert!(!format!("{found:?}").contains(COOKIE));
	}

	#[test]
	fn the_signed_in_accounts_cookie() {
		let cookie_target = format!("{TARGET}.ROBLOSECURITY{USER}");
		// as Studio writes them, UTF-8; UTF-16 and a trailing NUL read the same
		let entries = [(format!("{TARGET}userid"), USER.as_bytes().to_vec()), (cookie_target.clone(), COOKIE.as_bytes().to_vec())];
		assert_eq!(found_value(lookup(&entries)), COOKIE);
		let entries = [(format!("{TARGET}userid"), utf16(USER)), (cookie_target.clone(), utf16(&format!("{COOKIE}\0")))];
		assert_eq!(found_value(lookup(&entries)), COOKIE);
		// a `.ROBLOSECURITY=` prefix is dropped, as in env files
		let entries = [(format!("{TARGET}userid"), USER.as_bytes().to_vec()), (cookie_target.clone(), format!(".ROBLOSECURITY={COOKIE}").into_bytes())];
		assert_eq!(found_value(lookup(&entries)), COOKIE);
		// another account's cookie is never taken for the signed-in one's
		let entries = [(format!("{TARGET}userid"), USER.as_bytes().to_vec()), (format!("{TARGET}.ROBLOSECURITY999"), COOKIE.as_bytes().to_vec())];
		assert!(matches!(lookup(&entries), Lookup::NotFound));
	}

	#[test]
	fn without_a_user_id_the_one_account() {
		// a build that kept one account without its id
		assert_eq!(found_value(lookup(&[(format!("{TARGET}.ROBLOSECURITY"), COOKIE.as_bytes().to_vec())])), COOKIE);
		// the one account with a cookie; the other entries Studio keeps are not cookies
		let entries = [(format!("{TARGET}.ROBLOSECURITY{USER}"), COOKIE.as_bytes().to_vec()), (format!("{TARGET}accessToken{USER}"), b"other".to_vec())];
		assert_eq!(found_value(lookup(&entries)), COOKIE);
		// several: which is signed in is unknown
		let entries = [(format!("{TARGET}.ROBLOSECURITY1"), COOKIE.as_bytes().to_vec()), (format!("{TARGET}.ROBLOSECURITY2"), COOKIE.as_bytes().to_vec())];
		let several = lookup(&entries);
		assert!(matches!(several, Lookup::Unreadable(_)), "{}", several.status());
		assert!(matches!(lookup(&[]), Lookup::NotFound));
	}

	#[test]
	fn what_cannot_be_read_is_said_without_a_value() {
		let entries = [(format!("{TARGET}userid"), USER.as_bytes().to_vec()), (format!("{TARGET}.ROBLOSECURITY{USER}"), b"TEST VALUE WITH SPACES".to_vec())];
		let status = lookup(&entries).status();
		assert_eq!(status, "unreadable: the Windows Credential Manager entry holds no usable cookie");
		let failing = from_credentials(&|_| Err("Windows Credential Manager error 5".into()), &|_| Ok(Vec::new()));
		assert_eq!(failing.status(), "unreadable: Windows Credential Manager error 5");
	}

	#[test]
	fn the_registry_value_of_older_builds() {
		let value = format!("SEC::<YES>,EXP::<2099-01-01T00:00:00Z>,COOK::<{COOKIE}>");
		match from_registry_value(&value) {
			Lookup::Found(secret, Store::Registry) => assert_eq!(secret.expose(), COOKIE),
			other => panic!("{}", other.status()),
		}
		assert_eq!(from_registry_value("SEC::<YES>,EXP::<2099-01-01>").status(), "unreadable: the Windows registry entry holds no cookie");
		assert_eq!(from_registry_value("COOK::<A B>").status(), "unreadable: the Windows registry entry holds no usable cookie");
	}

	/// One cookie record: the header (size, flags, the strings' offsets, expiry and creation), then the strings.
	fn record(domain: &str, name: &str, value: &str, expiry: f64) -> Vec<u8> {
		const HEADER: usize = 56;
		let (mut offsets, mut strings) = (Vec::new(), Vec::new());
		for text in [domain, name, "/", value] {
			offsets.push((HEADER + strings.len()) as u32);
			strings.extend_from_slice(text.as_bytes());
			strings.push(0);
		}
		let mut record = Vec::new();
		for word in [(HEADER + strings.len()) as u32, 0, 1, 0, offsets[0], offsets[1], offsets[2], offsets[3], 0, 0] {
			record.extend(word.to_le_bytes());
		}
		record.extend(expiry.to_le_bytes());
		record.extend(0f64.to_le_bytes());
		record.extend(strings);
		record
	}

	fn page(records: &[Vec<u8>]) -> Vec<u8> {
		let mut page = vec![0, 0, 1, 0];
		page.extend((records.len() as u32).to_le_bytes());
		let mut offset = 8 + 4 * records.len() + 4;
		for record in records {
			page.extend((offset as u32).to_le_bytes());
			offset += record.len();
		}
		page.extend([0; 4]);
		for record in records {
			page.extend(record);
		}
		page
	}

	fn cookie_file(pages: &[Vec<u8>]) -> Vec<u8> {
		let mut file = b"cook".to_vec();
		file.extend((pages.len() as u32).to_be_bytes());
		for page in pages {
			file.extend((page.len() as u32).to_be_bytes());
		}
		for page in pages {
			file.extend(page);
		}
		// the checksum and the footer, which the reader does not need
		file.extend([0; 4]);
		file.extend([0x07, 0x17, 0x20, 0x05, 0, 0, 0, 0x4b]);
		file
	}

	fn parsed(file: &[u8]) -> Option<String> {
		parse_binary_cookies(file).unwrap().map(|secret| secret.expose().to_string())
	}

	#[test]
	fn the_roblox_cookie_in_a_binary_cookie_file() {
		let file = cookie_file(&[page(&[record(".example.com", ".ROBLOSECURITY", "TEST-OTHER-SITE", 9e8), record(".roblox.com", "RBXEventTracker", "TEST-NOT-IT", 9e8), record(".roblox.com", ".ROBLOSECURITY", COOKIE, 9e8)])]);
		assert_eq!(parsed(&file).as_deref(), Some(COOKIE));
		// a subdomain counts; lookalike domains do not
		let file = cookie_file(&[page(&[record("www.roblox.com", ".ROBLOSECURITY", COOKIE, 9e8)])]);
		assert_eq!(parsed(&file).as_deref(), Some(COOKIE));
		for domain in [".notroblox.com", "roblox.com.example", ".roblox.co"] {
			assert_eq!(parsed(&cookie_file(&[page(&[record(domain, ".ROBLOSECURITY", COOKIE, 9e8)])])), None, "{domain}");
		}
		// on another page, and the one that expires last of two
		let file = cookie_file(&[page(&[record(".roblox.com", ".ROBLOSECURITY", "TEST-OLDER", 8e8)]), page(&[record(".roblox.com", ".ROBLOSECURITY", COOKIE, 9e8)])]);
		assert_eq!(parsed(&file).as_deref(), Some(COOKIE));
		assert_eq!(parsed(&cookie_file(&[])), None);
		assert_eq!(parsed(&cookie_file(&[page(&[])])), None);
	}

	/// Studio's own entries in the file, as it writes them on macOS (seen in Studio 0.748's file: cookies of
	/// `www.roblox.com` named `/RobloxStudioAuth/userid` and `/RobloxStudioAuth/.ROBLOSECURITY<user id>`, and no
	/// cookie named `.ROBLOSECURITY` itself), read by the rule its Windows credentials are.
	#[test]
	fn studios_own_entries_in_a_binary_cookie_file() {
		const OTHER: &str = "TEST-OTHER-ACCOUNT-0123456789";
		let account = |id: &str, value: &str| record("www.roblox.com", &format!("/RobloxStudioAuth/.ROBLOSECURITY{id}"), value, 9e8);
		let user = |id: &str| record("www.roblox.com", "/RobloxStudioAuth/userid", id, 9e8);
		let noise = || record("www.roblox.com", &format!("/RobloxStudioAuth/accessToken{USER}"), "TEST-NOT-A-COOKIE", 9e8);
		// the signed-in account's cookie among several accounts', whatever order they are in and whatever page
		let file = cookie_file(&[page(&[record(".roblox.com", "RBXEventTracker", "TEST-NOT-IT", 9e8)]), page(&[account("99999", OTHER), noise(), user(USER), account(USER, COOKIE)])]);
		assert_eq!(parsed(&file).as_deref(), Some(COOKIE));
		// before a cookie named `.ROBLOSECURITY` itself
		let file = cookie_file(&[page(&[record(".roblox.com", ".ROBLOSECURITY", OTHER, 9e9), user(USER), account(USER, COOKIE)])]);
		assert_eq!(parsed(&file).as_deref(), Some(COOKIE));
		// a user id with no cookie of its own: the one kept without a user id, else `.ROBLOSECURITY` itself, else none
		assert_eq!(parsed(&cookie_file(&[page(&[user(USER), account("", COOKIE), account("99999", OTHER)])])).as_deref(), Some(COOKIE));
		assert_eq!(parsed(&cookie_file(&[page(&[user(USER), account("99999", OTHER), record(".roblox.com", ".ROBLOSECURITY", COOKIE, 9e8)])])).as_deref(), Some(COOKIE));
		assert_eq!(parsed(&cookie_file(&[page(&[user(USER), account("99999", OTHER)])])), None);
		// no user id: the one without a user id, else the one account that has a cookie; with several, none
		assert_eq!(parsed(&cookie_file(&[page(&[account("", COOKIE), account("99999", OTHER)])])).as_deref(), Some(COOKIE));
		assert_eq!(parsed(&cookie_file(&[page(&[noise(), account(USER, COOKIE)])])).as_deref(), Some(COOKIE));
		let several = parse_binary_cookies(&cookie_file(&[page(&[account(USER, COOKIE), account("99999", OTHER)])]));
		assert_eq!(several.as_ref().err().copied(), Some("several Studio accounts have a cookie in the macOS cookie file and none is marked as signed in"));
		assert!(!format!("{several:?}").contains(COOKIE) && !format!("{several:?}").contains(USER));
		// a user id that is not one names no account; another site's entries are not Studio's
		assert_eq!(parsed(&cookie_file(&[page(&[user("not-an-id"), account(USER, COOKIE)])])).as_deref(), Some(COOKIE));
		assert_eq!(parsed(&cookie_file(&[page(&[record("www.example.com", "/RobloxStudioAuth/userid", USER, 9e8), record("www.example.com", &format!("/RobloxStudioAuth/.ROBLOSECURITY{USER}"), COOKIE, 9e8)])])), None);
	}

	#[test]
	fn a_malformed_binary_cookie_file_is_an_error_not_a_panic() {
		let good = cookie_file(&[page(&[record(".roblox.com", ".ROBLOSECURITY", COOKIE, 9e8)])]);
		assert_eq!(parse_binary_cookies(b"nope").unwrap_err(), "the macOS cookie file is not a binary cookie file");
		// cut anywhere before its page ends, or any byte set to 0xff
		for length in 0..good.len() - 12 {
			let error = parse_binary_cookies(&good[..length]);
			assert!(error.is_err(), "cut at {length}");
			assert!(!format!("{error:?}").contains(COOKIE));
		}
		for at in 4..good.len() {
			let mut broken = good.clone();
			broken[at] = 0xff;
			let _ = parse_binary_cookies(&broken);
		}
		// a value that is no cookie value
		let file = cookie_file(&[page(&[record(".roblox.com", ".ROBLOSECURITY", "A B", 9e8)])]);
		assert_eq!(parse_binary_cookies(&file).unwrap_err(), "the macOS cookie file's .ROBLOSECURITY cookie holds no usable value");
	}

	/// The Windows calls on names no credential has: nothing is found, and nothing real is read.
	#[cfg(windows)]
	#[test]
	fn the_credential_manager_answers_a_name_it_lacks() {
		let name = format!("ror-assets-test-absent-{}", std::process::id());
		assert!(os::credential(&name).unwrap().is_none());
		assert!(os::credential_names(&format!("{name}*")).unwrap().is_empty());
	}
}
