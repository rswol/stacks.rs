use std::num::NonZeroU32;

use crate::address::AddressVersion;
use crate::address::StacksAddress;
use crate::crypto::bip32::KEY_BYTE_SIZE;
use crate::crypto::c32_address;
use crate::crypto::ExtendedPrivateKey;
use crate::Error;
use crate::StacksPrivateKey;
use crate::StacksPublicKey;

use rand::Rng;
use ring::aead;
use ring::pbkdf2;

pub(crate) const STX_DERIVATION_PATH: &str = "m/44'/5757'/0'/0";

pub type StacksAccounts = std::collections::HashMap<u32, StacksAccount>;

/// A `StacksAccount` instance, which contains a public key, a private key, and a derivation index.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct StacksAccount {
    pub index: u32,
    pub public_key: StacksPublicKey,
    pub private_key: StacksPrivateKey,
}

impl StacksAccount {
    /// Creates a new `StacksAccount`.
    fn new(index: u32, public_key: StacksPublicKey, private_key: StacksPrivateKey) -> Self {
        Self {
            index,
            public_key,
            private_key,
        }
    }

    /// Derives an account from a root key and an index.
    fn derive(root: &ExtendedPrivateKey, index: u32) -> Result<Self, Error> {
        let child = root.derive(STX_DERIVATION_PATH)?.child(index.into())?;
        let public_key = child.public_key();
        let private_key = child.private_key;
        Ok(Self::new(index, public_key, private_key))
    }

    /// Returns the address of the account for a given version.
    pub fn get_address(&self, version: AddressVersion) -> Result<String, Error> {
        let address = StacksAddress::from_public_key(self.public_key, None)?;
        let c32 = c32_address(address.as_bytes(), version as u8)?;
        Ok(c32)
    }
}

/// A parent `StacksWallet`, which contains a root key and a map of derived accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StacksWallet {
    root_key: ExtendedPrivateKey,
    accounts: StacksAccounts,
}

impl StacksWallet {
    /// Creates a new `StacksWallet`.
    fn new(root_key: ExtendedPrivateKey, accounts: StacksAccounts) -> Self {
        Self { root_key, accounts }
    }

    /// Creates a new `StacksWallet` from a secret key / mnemonic phrase.
    pub fn from_secret_key(secret_key: impl Into<String>) -> Result<Self, Error> {
        let secret_key = secret_key.into();

        let mnemonic = bip39::Mnemonic::parse(secret_key)?;
        let seed = mnemonic.to_seed_normalized("");
        let root_key = ExtendedPrivateKey::from_seed(seed)?;
        Ok(Self::new(root_key, StacksAccounts::new()))
    }

    /// Gets an account by derivation index.
    pub fn get_account(&mut self, index: u32) -> Result<StacksAccount, Error> {
        if let Some(account) = self.accounts.get(&index) {
            Ok(*account)
        } else {
            let account = StacksAccount::derive(&self.root_key, index)?;
            self.set_account(index, account);
            Ok(account)
        }
    }

    /// Sets an account by derivation index.
    pub fn set_account(&mut self, index: u32, account: StacksAccount) {
        self.accounts.insert(index, account);
    }

    /// Encrypts the wallet with a passphrase.
    pub fn encrypt_key(&self, passphrase: &str) -> Result<Vec<u8>, Error> {
        let mut salt = [0u8; 16];
        let mut rng = rand::thread_rng();

        salt.copy_from_slice(&rng.gen::<[u8; 16]>()[..]);

        #[allow(clippy::unwrap_used)]
        let n_iter = NonZeroU32::new(100_000).unwrap();
        let mut key_and_nonce = [0u8; 16 + aead::NONCE_LEN];

        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA512,
            n_iter,
            &salt,
            passphrase.as_bytes(),
            &mut key_and_nonce,
        );

        let enc_key = &key_and_nonce[..16];
        let mut nonce = [0u8; aead::NONCE_LEN];
        nonce.copy_from_slice(&key_and_nonce[16..]);

        let key = aead::UnboundKey::new(&aead::AES_128_GCM, enc_key)?;
        let key = aead::LessSafeKey::new(key);
        let nonce = aead::Nonce::assume_unique_for_key(nonce);

        let mut data = vec![0u8; 0];
        data.extend_from_slice(&self.root_key.chain_code[..]);
        data.extend(self.root_key.private_key.secret_bytes());

        key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut data)?;

        // result is salt + ciphertext + tag
        let mut result = salt.to_vec();
        result.extend_from_slice(&data);

        Ok(result)
    }

    /// Creates a wallet from an encrypted key and a passphrase.
    pub fn from_encrypted_key(passphrase: &str, data: &[u8]) -> Result<Self, Error> {
        let salt = &data[..16];
        let ciphertext = &data[16..];

        #[allow(clippy::unwrap_used)]
        let n_iter = NonZeroU32::new(100_000).unwrap();
        let mut key_and_nonce = [0u8; 16 + aead::NONCE_LEN];

        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA512,
            n_iter,
            salt,
            passphrase.as_bytes(),
            &mut key_and_nonce,
        );

        let enc_key = &key_and_nonce[..16];
        let mut nonce = [0u8; aead::NONCE_LEN];
        nonce.copy_from_slice(&key_and_nonce[16..]);

        let key = aead::UnboundKey::new(&aead::AES_128_GCM, enc_key)?;
        let key = aead::LessSafeKey::new(key);
        let nonce = aead::Nonce::assume_unique_for_key(nonce);

        let mut data = ciphertext.to_vec();
        key.open_in_place(nonce, aead::Aad::empty(), &mut data)?;

        let chain_code: [u8; KEY_BYTE_SIZE] = data[..KEY_BYTE_SIZE].try_into()?;
        let private_key = StacksPrivateKey::from_slice(&data[KEY_BYTE_SIZE..data.len() - 16])?;
        let root_key = ExtendedPrivateKey {
            private_key,
            chain_code,
            depth: 0,
        };

        Ok(Self::new(root_key, StacksAccounts::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_address() {
        let secret_key = "sound idle panel often situate develop unit text design antenna vendor screen opinion balcony share trigger accuse scatter visa uniform brass update opinion media";
        let mut wallet = StacksWallet::from_secret_key(secret_key).unwrap();
        let account = wallet.get_account(0).unwrap();

        let mainnet_p2pkh = account.get_address(AddressVersion::MainnetP2PKH).unwrap();
        let mainnet_p2sh = account.get_address(AddressVersion::MainnetP2SH).unwrap();
        let testnet_p2pkh = account.get_address(AddressVersion::TestnetP2PKH).unwrap();
        let testnet_p2sh = account.get_address(AddressVersion::TestnetP2SH).unwrap();

        let expected_mainnet_p2pkh = "SP384CVPNDTYA0E92TKJZQTYXQHNZSWGCAG7SAPVB";
        let expected_mainnet_p2sh = "SM384CVPNDTYA0E92TKJZQTYXQHNZSWGCAGRD22C9";
        let expected_testnet_p2pkh = "ST384CVPNDTYA0E92TKJZQTYXQHNZSWGCAH0ER64E";
        let expected_testnet_p2sh = "SN384CVPNDTYA0E92TKJZQTYXQHNZSWGCAKNRHMGW";

        assert_eq!(mainnet_p2pkh, expected_mainnet_p2pkh);
        assert_eq!(mainnet_p2sh, expected_mainnet_p2sh);
        assert_eq!(testnet_p2pkh, expected_testnet_p2pkh);
        assert_eq!(testnet_p2sh, expected_testnet_p2sh);
    }

    #[test]
    fn test_account_address_index() {
        let secret_key = "sound idle panel often situate develop unit text design antenna vendor screen opinion balcony share trigger accuse scatter visa uniform brass update opinion media";
        let mut wallet = StacksWallet::from_secret_key(secret_key).unwrap();
        let account = wallet.get_account(1).unwrap();

        let mainnet_p2pkh = account.get_address(AddressVersion::MainnetP2PKH).unwrap();
        let mainnet_p2sh = account.get_address(AddressVersion::MainnetP2SH).unwrap();
        let testnet_p2pkh = account.get_address(AddressVersion::TestnetP2PKH).unwrap();
        let testnet_p2sh = account.get_address(AddressVersion::TestnetP2SH).unwrap();

        let expected_mainnet_p2pkh = "SP23K7K2V45JFZVBMQBE8R0PP8SQG7HZF9473KBD";
        let expected_mainnet_p2sh = "SM23K7K2V45JFZVBMQBE8R0PP8SQG7HZFB7DZ2RK";
        let expected_testnet_p2pkh = "ST23K7K2V45JFZVBMQBE8R0PP8SQG7HZFA6Z68VE";
        let expected_testnet_p2sh = "SN23K7K2V45JFZVBMQBE8R0PP8SQG7HZFAFNYMDJ";

        assert_eq!(mainnet_p2pkh, expected_mainnet_p2pkh);
        assert_eq!(mainnet_p2sh, expected_mainnet_p2sh);
        assert_eq!(testnet_p2pkh, expected_testnet_p2pkh);
        assert_eq!(testnet_p2sh, expected_testnet_p2sh);
    }

    #[test]
    fn encrypt_key() {
        let secret_key = "sound idle panel often situate develop unit text design antenna vendor screen opinion balcony share trigger accuse scatter visa uniform brass update opinion media";
        let wallet = StacksWallet::from_secret_key(secret_key).unwrap();
        let data = wallet.encrypt_key("hello world").unwrap();
        let wallet2 = StacksWallet::from_encrypted_key("hello world", &data).unwrap();
        assert_eq!(wallet2, wallet);
    }
}
