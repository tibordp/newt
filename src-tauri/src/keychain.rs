use crate::common::Error;

const SERVICE_NAME: &str = "com.newt.credentials";

fn entry(key: &str) -> Result<keyring::Entry, Error> {
    keyring::Entry::new(SERVICE_NAME, key).map_err(|e| Error::Custom(e.to_string()))
}

#[tauri::command]
pub fn keychain_get(key: String) -> Result<Option<String>, Error> {
    let entry = entry(&key)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Custom(e.to_string())),
    }
}

#[tauri::command]
pub fn keychain_set(key: String, value: String) -> Result<(), Error> {
    let entry = entry(&key)?;
    entry
        .set_password(&value)
        .map_err(|e| Error::Custom(e.to_string()))
}

#[tauri::command]
pub fn keychain_delete(key: String) -> Result<(), Error> {
    let entry = entry(&key)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Custom(e.to_string())),
    }
}
