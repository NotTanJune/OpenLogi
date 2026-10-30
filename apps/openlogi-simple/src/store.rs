//! The saved configuration and the one way this app writes it.
//!
//! [`Store`] owns a [`Config`] together with the [`ConfigFile`] it was read
//! from. A [`Store::commit`] applies one [`Edit`] to a copy, saves the copy
//! through the file (which refuses to overwrite edits made after it was read),
//! and adopts the copy only once the save landed. A failed save therefore
//! leaves the store holding exactly what is on disk, the same rollback the
//! upstream GUI's `ConfigState::persist` gives.

use std::path::Path;

use openlogi_core::config::{Config, ConfigError, ConfigFile, ConfigRevision};

use crate::model::{Edit, ModelError, apply};

/// Why a commit did not happen.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The edit itself was refused.
    #[error(transparent)]
    Edit(#[from] ModelError),
    /// The device has no persistent key, so there is no entry to write.
    #[error("this device has no stable identity yet, so its settings cannot be saved")]
    Transient,
    /// Loading or saving `config.toml` failed. After a
    /// [`ConfigError::Conflict`] call [`Store::refresh`] to adopt the file as
    /// it is now.
    #[error(transparent)]
    File(#[from] ConfigError),
}

/// The configuration and its file.
#[derive(Debug)]
pub struct Store {
    config: Config,
    file: ConfigFile,
}

impl Store {
    /// Read the user's `config.toml`; a missing file reads as the defaults.
    ///
    /// # Errors
    ///
    /// The file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, StoreError> {
        let (config, file) = ConfigFile::load_or_default()?;
        Ok(Self { config, file })
    }

    /// Read the config at `path`.
    ///
    /// # Errors
    ///
    /// The file exists but cannot be read or parsed.
    pub fn load_from(path: &Path) -> Result<Self, StoreError> {
        let (config, file) = ConfigFile::load_from_path(path)?;
        Ok(Self { config, file })
    }

    /// The configuration as last read or saved.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The revision of the file as this store last read or saved it, which
    /// `reload_saved_config` names so the agent can tell a later edit.
    #[must_use]
    pub fn revision(&self) -> ConfigRevision {
        self.file.revision()
    }

    /// Apply `edit` for `device` and save it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Transient`] for a device without a persistent key,
    /// [`StoreError::Edit`] for a refused edit, [`StoreError::File`] when the
    /// save failed. Nothing is saved or adopted on any of them.
    pub fn commit(
        &mut self,
        device: &crate::devices::Device,
        edit: Edit,
    ) -> Result<(), StoreError> {
        if !device.persistent {
            return Err(StoreError::Transient);
        }
        let mut next = self.config.clone();
        apply(&mut next, &device.key, edit)?;
        self.file.save(&next)?;
        self.config = next;
        Ok(())
    }

    /// Read the file again and adopt it, dropping what this store held. For
    /// recovery after a [`ConfigError::Conflict`].
    ///
    /// # Errors
    ///
    /// The file cannot be read or parsed; the store is left as it was.
    pub fn refresh(&mut self) -> Result<(), StoreError> {
        let (config, file) = self.file.reload()?;
        self.config = config;
        self.file = file;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::binding::{Action, Binding, ButtonId};
    use openlogi_core::hid::Dpi;

    use crate::devices::Device;
    use crate::model::Button;

    use super::*;

    fn device(key: &str, persistent: bool) -> Device {
        Device {
            key: key.to_owned(),
            persistent,
            name: "Test mouse".to_owned(),
            route: None,
            route_key: "route".to_owned(),
            online: true,
            battery: None,
            capabilities: None,
        }
    }

    /// A config as text, for comparing: `Config` has no `PartialEq`.
    fn text(config: &Config) -> String {
        toml::to_string(config).expect("a config serializes")
    }

    fn back_to(action: Action) -> Edit {
        Edit::Binding {
            button: Button::Back,
            action,
        }
    }

    #[test]
    fn a_commit_is_on_disk_and_in_the_store() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config.toml");
        let mut store = Store::load_from(&path).expect("a missing file loads as defaults");
        let before = store.revision();

        store
            .commit(&device("unit:aa", true), back_to(Action::Copy))
            .expect("saves");

        assert_ne!(store.revision(), before, "the revision follows the save");
        assert_eq!(
            store
                .config()
                .stored_bindings("unit:aa")
                .get(&ButtonId::Back),
            Some(&Binding::Single(Action::Copy))
        );
        let reread = Store::load_from(&path).expect("the saved file loads");
        assert_eq!(text(reread.config()), text(store.config()));
        assert_eq!(reread.revision(), store.revision());
    }

    #[test]
    fn commits_to_two_devices_do_not_touch_each_other() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut store = Store::load_from(&dir.path().join("config.toml")).expect("loads");
        store
            .commit(&device("unit:aa", true), back_to(Action::Copy))
            .expect("saves");
        store
            .commit(&device("unit:bb", true), Edit::Dpi(Dpi::new(800)))
            .expect("saves");
        assert_eq!(store.config().dpi("unit:aa"), None);
        assert_eq!(store.config().dpi("unit:bb"), Some(Dpi::new(800)));
        assert!(store.config().stored_bindings("unit:bb").is_empty());
    }

    #[test]
    fn a_transient_device_and_a_refused_edit_write_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config.toml");
        let mut store = Store::load_from(&path).expect("loads");
        let before = store.revision();

        let transient = store.commit(&device("runtime:x", false), back_to(Action::Copy));
        assert!(
            matches!(transient, Err(StoreError::Transient)),
            "{transient:?}"
        );
        let refused = store.commit(&device("unit:aa", true), Edit::Dpi(Dpi::new(0)));
        assert!(
            matches!(refused, Err(StoreError::Edit(ModelError::InvalidDpi))),
            "{refused:?}"
        );

        assert_eq!(store.revision(), before);
        assert!(!path.exists(), "nothing reached the disk");
    }

    #[test]
    fn a_save_that_loses_to_an_outside_edit_leaves_the_store_unchanged_until_refreshed() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("config.toml");
        let mut store = Store::load_from(&path).expect("loads");
        store
            .commit(&device("unit:aa", true), back_to(Action::Copy))
            .expect("saves");

        // Another writer (the CLI, a hand edit) changes the file.
        let mut other = Store::load_from(&path).expect("loads");
        other
            .commit(&device("unit:aa", true), back_to(Action::Paste))
            .expect("saves");

        let held = text(store.config());
        let result = store.commit(&device("unit:aa", true), back_to(Action::Cut));
        assert!(
            matches!(result, Err(StoreError::File(ConfigError::Conflict { .. }))),
            "{result:?}"
        );
        assert_eq!(text(store.config()), held, "a failed save adopts nothing");

        store.refresh().expect("the file reads");
        assert_eq!(
            store
                .config()
                .stored_bindings("unit:aa")
                .get(&ButtonId::Back),
            Some(&Binding::Single(Action::Paste))
        );
        store
            .commit(&device("unit:aa", true), back_to(Action::Cut))
            .expect("saves again");
        assert_eq!(
            text(Store::load_from(&path).expect("loads").config()),
            text(store.config())
        );
    }
}
