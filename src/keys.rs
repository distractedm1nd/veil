use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result};
use bip0039::{Count, English, Mnemonic};
use secrecy::SecretVec;
use zeroize::Zeroizing;

pub struct KeyStore {
    identity: age::x25519::Identity,
}

impl KeyStore {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Self {
                identity: Zeroizing::new(value)
                    .trim()
                    .parse()
                    .map_err(anyhow::Error::msg)?,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                let identity = age::x25519::Identity::generate();
                let mut file = private_file(path)?;
                file.write_all(identity.to_string().expose_secret().as_bytes())?;
                file.sync_all()?;
                Ok(Self { identity })
            }
            Err(error) => Err(error).context("read wallet encryption identity"),
        }
    }

    pub fn encrypt(&self, mnemonic: &str) -> Result<Vec<u8>> {
        let recipient = self.identity.to_public();
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?;
        let mut result = Vec::new();
        let mut writer = encryptor.wrap_output(&mut result)?;
        writer.write_all(mnemonic.as_bytes())?;
        writer.finish()?;
        Ok(result)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Zeroizing<String>> {
        let decryptor = age::Decryptor::new(ciphertext)?;
        let mut reader =
            decryptor.decrypt(std::iter::once(&self.identity as &dyn age::Identity))?;
        let mut mnemonic = Zeroizing::new(String::new());
        reader.read_to_string(&mut mnemonic)?;
        Ok(mnemonic)
    }
}

pub fn generate_mnemonic() -> Zeroizing<String> {
    Zeroizing::new(
        Mnemonic::<English>::generate(Count::Words24)
            .phrase()
            .to_owned(),
    )
}

pub fn seed(mnemonic: &str) -> Result<SecretVec<u8>> {
    let mnemonic = Mnemonic::<English>::from_phrase(mnemonic)?;
    Ok(SecretVec::new(mnemonic.to_seed("").to_vec()))
}

pub fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
