//! The `synchronizer` module is the core component of the `mmap-sync` library, providing a `Synchronizer` struct for concurrent data access.
//!
//! The `Synchronizer` offers a simple interface for reading and writing data from/to shared memory. It uses memory-mapped files and wait-free synchronization to provide high concurrency wait-free reads over a single writer instance. This design is inspired by the [Left-Right concurrency control technique](https://github.com/pramalhe/ConcurrencyFreaks/blob/master/papers/left-right-2014.pdf), allowing for efficient and flexible inter-process communication.
//!
//! This implementation uses protocol buffers (via [Prost](https://github.com/tokio-rs/prost)) for serialization and deserialization of data.

use std::ffi::OsStr;
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::time::Duration;

use prost::Message;
use thiserror::Error;
use wyhash::WyHash;

use crate::data::DataContainer;
use crate::guard::{ReadGuard, ReadResult};
use crate::instance::InstanceVersion;
use crate::state::StateContainer;
use crate::synchronizer::SynchronizerError::*;

/// `Synchronizer` is a concurrency primitive that manages data access between a single writer process and multiple reader processes.
///
/// It coordinates the access to two data files that store the shared data. A state file, also memory-mapped, stores the index of the current data file and the number of active readers for each index, updated via atomic instructions.
pub struct Synchronizer<H: Hasher + Default = WyHash> {
    /// Container storing state mmap
    state_container: StateContainer,
    /// Container storing data mmap
    data_container: DataContainer,
    /// Hasher used for checksum calculation
    build_hasher: BuildHasherDefault<H>,
    /// Re-usable buffer for serialization
    serialize_buffer: Vec<u8>,
}

/// `SynchronizerError` enumerates all possible errors returned by this library.
/// These errors mainly represent the failures that might occur during reading or writing
/// operations in data or state files.
#[derive(Error, Debug)]
pub enum SynchronizerError {
    /// An error occurred while writing to the data file.
    #[error("error writing data file: {0}")]
    FailedDataWrite(std::io::Error),
    /// An error occurred while reading from the data file.
    #[error("error reading data file: {0}")]
    FailedDataRead(std::io::Error),
    /// An error occurred while reading from the state file.
    #[error("error reading state file: {0}")]
    FailedStateRead(std::io::Error),
    /// An error occurred while writing an entity.
    #[error("error writing entity")]
    FailedEntityWrite,
    /// An error occurred while reading an entity.
    #[error("error reading entity")]
    FailedEntityRead,
    /// The state was not properly initialized.
    #[error("uninitialized state")]
    UninitializedState,
    /// The instance version parameters were invalid.
    #[error("invalid instance version params")]
    InvalidInstanceVersionParams,
}

impl Synchronizer {
    /// Create new instance of `Synchronizer` using given `path_prefix` and default template parameters
    pub fn new(path_prefix: &OsStr) -> Self {
        Self::with_params(path_prefix)
    }
}

impl<H: Hasher + Default> Synchronizer<H> {
    /// Create new instance of `Synchronizer` using given `path_prefix` and template parameters
    pub fn with_params(path_prefix: &OsStr) -> Self {
        Synchronizer {
            state_container: StateContainer::new(path_prefix),
            data_container: DataContainer::new(path_prefix),
            build_hasher: BuildHasherDefault::default(),
            serialize_buffer: Vec::new(),
        }
    }

    /// Writes a given `entity` into the next available data file.
    ///
    /// Returns the number of bytes written to the data file and a boolean flag, for diagnostic
    /// purposes, indicating whether the reader counter was reset due to a reader exiting without
    /// decrementing it.
    ///
    /// # Parameters
    /// - `entity`: The entity to be written to the data file.
    /// - `grace_duration`: The maximum period to wait for readers to finish before resetting the
    ///                     reader count to 0. This handles scenarios where a reader process has
    ///                     crashed or exited abnormally, failing to decrement the reader count.
    ///                     After the `grace_duration` has elapsed, if there are still active
    ///                     readers, the reader count is reset to 0 to restore synchronization state.
    ///
    /// # Returns
    /// A result containing a tuple of the number of bytes written and a boolean indicating whether
    /// the reader count was reset, or a `SynchronizerError` if the operation fails.
    pub fn write<T>(
        &mut self,
        entity: &T,
        grace_duration: Duration,
    ) -> Result<(usize, bool), SynchronizerError>
    where
        T: Message,
    {
        self.serialize_buffer.clear();

        // serialize given entity into bytes
        entity
            .encode(&mut self.serialize_buffer)
            .map_err(|_| FailedEntityWrite)?;

        // fetch current state from mapped memory
        let state = self.state_container.state(true)?;

        // calculate data checksum
        let mut hasher = self.build_hasher.build_hasher();
        hasher.write(&self.serialize_buffer);
        let checksum = hasher.finish();

        // acquire next available data file idx and write data to it
        let (new_idx, reset) = state.acquire_next_idx(grace_duration);
        let new_version = InstanceVersion::new(new_idx, self.serialize_buffer.len(), checksum)?;
        let size = self
            .data_container
            .write(&self.serialize_buffer, new_version)?;

        // switch readers to new version
        state.switch_version(new_version);

        Ok((size, reset))
    }

    /// Write raw data bytes representing type `T` into the next available data file.
    /// Returns number of bytes written to data file and a boolean flag, for diagnostic purposes,
    /// indicating that we have reset our readers counter after a reader died without decrementing it.
    pub fn write_raw<T>(
        &mut self,
        data: &[u8],
        grace_duration: Duration,
    ) -> Result<(usize, bool), SynchronizerError>
    where
        T: Message,
    {
        // fetch current state from mapped memory
        let state = self.state_container.state(true)?;

        // calculate data checksum
        let mut hasher = self.build_hasher.build_hasher();
        hasher.write(data);
        let checksum = hasher.finish();

        // acquire next available data file idx and write data to it
        let (new_idx, reset) = state.acquire_next_idx(grace_duration);
        let new_version = InstanceVersion::new(new_idx, data.len(), checksum)?;
        let size = self.data_container.write(data, new_version)?;

        // switch readers to new version
        state.switch_version(new_version);

        Ok((size, reset))
    }

    /// Reads and returns an `entity` struct from mapped memory wrapped in `ReadGuard`.
    ///
    /// # Safety
    ///
    /// This method is marked as unsafe due to the potential for memory corruption if the returned
    /// result is used beyond the `grace_duration` set in the `write` method. The caller must ensure
    /// the `ReadGuard` (and any references derived from it) are dropped before this time period
    /// elapses to ensure safe operation.
    pub unsafe fn read<T>(&mut self) -> Result<ReadResult<T>, SynchronizerError>
    where
        T: Message + Default,
    {
        // fetch current state from mapped memory
        let state = self.state_container.state(false)?;

        // fetch current version
        let version = state.version()?;

        // create and lock state guard for reading
        let guard = ReadGuard::new(state, version)?;

        // fetch data for current version from mapped memory
        let (data, switched) = self.data_container.data(version)?;

        // deserialize entity from data
        let mut entity = T::default();
        entity.merge(data).map_err(|_| FailedEntityRead)?;

        Ok(ReadResult::new(guard, entity, switched))
    }

    /// Returns current `InstanceVersion` stored within the state, useful for detecting
    /// whether synchronized `entity` has changed.
    pub fn version(&mut self) -> Result<InstanceVersion, SynchronizerError> {
        // fetch current state from mapped memory
        let state = self.state_container.state(false)?;

        // fetch current version
        state.version()
    }
}

#[cfg(test)]
mod tests {
    use crate::synchronizer::Synchronizer;
    use prost::Message;
    use rand::distributions::Uniform;
    use rand::prelude::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    #[derive(Message, Clone, PartialEq)]
    struct MockEntity {
        #[prost(uint32, tag = "1")]
        version: u32,
        #[prost(map = "uint64, message", tag = "2")]
        map: HashMap<u64, FloatVec>,
    }

    #[derive(Message, Clone, PartialEq)]
    struct FloatVec {
        #[prost(float, repeated, tag = "1")]
        values: Vec<f32>,
    }

    struct MockEntityGenerator {
        rng: StdRng,
    }

    impl MockEntityGenerator {
        fn new(seed: u8) -> Self {
            MockEntityGenerator {
                rng: StdRng::from_seed([seed; 32]),
            }
        }

        fn gen(&mut self, n: usize) -> MockEntity {
            let mut entity = MockEntity {
                version: self.rng.gen(),
                map: HashMap::new(),
            };
            let range = Uniform::<f32>::from(0.0..100.0);
            for _ in 0..n {
                let key: u64 = self.rng.gen();
                let n_vals = self.rng.gen::<usize>() % 20;
                let vals: Vec<f32> = (0..n_vals).map(|_| self.rng.sample(range)).collect();
                entity
                    .map
                    .insert(key, super::tests::FloatVec { values: vals });
            }
            entity
        }
    }

    #[test]
    fn test_synchronizer() {
        let path = "/tmp/synchro_test";
        let state_path = path.to_owned() + "_state";
        let data_path_0 = path.to_owned() + "_data_0";
        let data_path_1 = path.to_owned() + "_data_1";

        // clean up test files before tests
        fs::remove_file(&state_path).unwrap_or_default();
        fs::remove_file(&data_path_0).unwrap_or_default();
        fs::remove_file(&data_path_1).unwrap_or_default();

        // create writer and reader synchronizers
        let mut writer = Synchronizer::new(path.as_ref());
        let mut reader = Synchronizer::new(path.as_ref());

        // use deterministic random generator for reproducible results
        let mut entity_generator = MockEntityGenerator::new(3);

        // check that `read` returns error when writer didn't write yet
        let res = unsafe { reader.read::<MockEntity>() };
        assert!(res.is_err());
        assert_eq!(
            res.err().unwrap().to_string(),
            "error reading state file: No such file or directory (os error 2)"
        );
        assert!(!Path::new(&state_path).exists());

        // check if can write entity with correct size
        let entity = entity_generator.gen(100);
        let (size, reset) = writer.write(&entity, Duration::from_secs(1)).unwrap();
        assert!(size > 0);
        assert_eq!(reset, false);
        assert!(Path::new(&state_path).exists());
        assert!(!Path::new(&data_path_1).exists());

        // check that first time scoped `read` works correctly and switches the data
        fetch_and_assert_entity(&mut reader, &entity, true);

        // check that second time scoped `read` works correctly and doesn't switch the data
        fetch_and_assert_entity(&mut reader, &entity, false);

        // check if can write entity again
        let entity = entity_generator.gen(200);
        let (size, reset) = writer.write(&entity, Duration::from_secs(1)).unwrap();
        assert!(size > 0);
        assert_eq!(reset, false);
        assert!(Path::new(&state_path).exists());
        assert!(Path::new(&data_path_0).exists());
        assert!(Path::new(&data_path_1).exists());

        // check that another scoped `read` works correctly and switches the data
        fetch_and_assert_entity(&mut reader, &entity, true);

        // write entity twice to switch to the same `idx` without any reads in between
        let entity = entity_generator.gen(100);
        let (size, reset) = writer.write(&entity, Duration::from_secs(1)).unwrap();
        assert!(size > 0);
        assert_eq!(reset, false);

        let entity = entity_generator.gen(200);
        let (size, reset) = writer.write(&entity, Duration::from_secs(1)).unwrap();
        assert!(size > 0);
        assert_eq!(reset, false);

        fetch_and_assert_entity(&mut reader, &entity, true);
    }

    fn fetch_and_assert_entity(
        synchronizer: &mut Synchronizer,
        expected_entity: &MockEntity,
        expected_is_switched: bool,
    ) {
        let actual_entity = unsafe { synchronizer.read::<MockEntity>().unwrap() };
        assert_eq!(actual_entity.map, expected_entity.map);
        assert_eq!(actual_entity.version, expected_entity.version);
        assert_eq!(actual_entity.is_switched(), expected_is_switched);
    }
}
