#![deny(missing_docs)]

// N.B. requires nightly rustdoc to document until intra-doc links are stabilized.
//! Collect information that allows you to build a crate the same way that docs.rs would.
//!
//! This library is intended for use in docs.rs and crater, but might be helpful to others.
//! Here is an example use of the crate:
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::process::Command;
//! use metadata::Metadata;
//!
//! // First, we need to parse Cargo.toml.
//! let source_root = env!("CARGO_MANIFEST_DIR");
//! let metadata = Metadata::from_crate_root(&source_root)?;
//!
//! // Next, learn what arguments we need to pass to `cargo`.
//! let targets = metadata.targets();
//! let mut cargo_args = metadata.cargo_args();
//! cargo_args.push(targets.default_target.into());
//!
//! // Now, set up the `Command`
//! let mut cmd = Command::new("cargo");
//! cmd.args(cargo_args);
//! for (key, value) in metadata.environment_variables() {
//!     cmd.env(key, value);
//! }
//!
//! // Finally, run `cargo doc` on the directory.
//! let result = cmd.output()?;
//! # Ok(())
//! # }
//! ```

use std::collections::{HashSet, HashMap};
use std::io;
use std::path::Path;

use thiserror::Error;
use toml::{map::Map, Value};

/// The target that `metadata` is being built for.
///
/// This is directly passed on from the Cargo [`TARGET`] variable.
///
/// [`TARGET`]: https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
pub const HOST_TARGET: &str = env!("DOCS_RS_METADATA_HOST_TARGET");
/// The targets that are built if no `targets` section is specified.
///
/// Currently, this is guaranteed to have only [tier one] targets.
/// However, it may not contain all tier one targets.
///
/// [tier one]: https://doc.rust-lang.org/nightly/rustc/platform-support.html#tier-1
pub const DEFAULT_TARGETS: &[&str] = &[
    "i686-pc-windows-msvc",
    "i686-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
];

/// The possible errors for [`Metadata::from_crate_root`].
#[derive(Debug, Error)]
pub enum MetadataError {
    /// The error returned when the manifest could not be read.
    #[error("failed to read manifest from disk")]
    IO(#[from] io::Error),
    /// The error returned when the manifest could not be parsed.
    #[error("failed to parse manifest")]
    Parse(#[from] toml::de::Error),
}

/// Metadata to set for custom builds.
///
/// This metadata is read from `[package.metadata.docs.rs]` table in `Cargo.toml`.
///
/// An example metadata:
///
/// ```text
/// [package]
/// name = "test"
///
/// [package.metadata.docs.rs]
/// features = [ "feature1", "feature2" ]
/// all-features = true
/// no-default-features = true
/// default-target = "x86_64-unknown-linux-gnu"
/// targets = [ "x86_64-apple-darwin", "x86_64-pc-windows-msvc" ]
/// rustc-args = [ "--example-rustc-arg" ]
/// rustdoc-args = [ "--example-rustdoc-arg" ]
/// ```
///
/// You can define one or more fields in your `Cargo.toml`.
pub struct Metadata {
    /// List of features to pass on to `cargo`.
    ///
    /// By default, docs.rs will only build default features.
    pub features: Option<Vec<String>>,

    /// Whether to pass `--all-features` to `cargo`.
    pub all_features: bool,

    /// Whether to pass `--no-default-features` to `cargo`.
    //
    /// By default, Docs.rs will build default features.
    /// Set `no-default-fatures` to `false` if you want to build only certain features.
    pub no_default_features: bool,

    /// See [`BuildTargets`].
    default_target: Option<String>,
    targets: Option<Vec<String>>,

    /// List of command line arguments for `rustc`.
    pub rustc_args: Option<Vec<String>>,

    /// List of command line arguments for `rustdoc`.
    pub rustdoc_args: Option<Vec<String>>,
}

/// The targets that should be built for a crate.
///
/// The `default_target` is the target to be used as the home page for that crate.
///
/// # See also
/// - [`Metadata::targets`]
pub struct BuildTargets<'a> {
    /// The target that should be built by default.
    ///
    /// If `default_target` is unset and `targets` is non-empty,
    /// the first element of `targets` will be used as the `default_target`.
    /// Otherwise, this defaults to [`HOST_TARGET`].
    pub default_target: &'a str,

    /// Which targets should be built.
    ///
    /// If you want a crate to build only for specific targets,
    /// set `targets` to the list of targets to build, in addition to `default-target`.
    ///
    /// If `targets` is not set, all [`DEFAULT_TARGETS`] will be built.
    /// If `targets` is set to an empty array, only the default target will be built.
    /// If `targets` is set to a non-empty array but `default_target` is not set,
    ///    the first element will be treated as the default.
    pub other_targets: HashSet<&'a str>,
}

impl Metadata {
    /// Read the `Cargo.toml` from a source directory, then parse the build metadata.
    ///
    /// If both `Cargo.toml` and `Cargo.toml.orig` exist in the directory,
    /// `Cargo.toml.orig` will take precedence.
    ///
    /// If you already have the path to a TOML file, use [`Metadata::from_manifest`] instead.
    pub fn from_crate_root<P: AsRef<Path>>(source_dir: P) -> Result<Metadata, MetadataError> {
        let source_dir = source_dir.as_ref();
        for &c in &["Cargo.toml.orig", "Cargo.toml"] {
            let manifest_path = source_dir.join(c);
            if manifest_path.exists() {
                return Metadata::from_manifest(manifest_path);
            }
        }

        Err(io::Error::new(io::ErrorKind::NotFound, "no Cargo.toml").into())
    }

    /// Read the given file into a string, then parse the build metadata.
    ///
    /// If you already have the TOML as a string, use [`from_str`] instead.
    /// If you just want the default settings, use [`Metadata::default()`][Default::default].
    ///
    /// [`from_str`]: std::str::FromStr
    pub fn from_manifest<P: AsRef<Path>>(path: P) -> Result<Metadata, MetadataError> {
        use std::{str::FromStr, fs};
        let buf = fs::read_to_string(path)?;
        Metadata::from_str(&buf).map_err(Into::into)
    }

    /// Return the targets that should be built.
    ///
    /// The `default_target` will never be one of the `other_targets`.
    pub fn targets(&self) -> BuildTargets<'_> {
        let default_target = self
            .default_target
            .as_deref()
            // Use the first element of `targets` if `default_target` is unset and `targets` is non-empty
            .or_else(|| {
                self.targets
                    .as_ref()
                    .and_then(|targets| targets.iter().next().map(String::as_str))
            })
            .unwrap_or(HOST_TARGET);

        // Let people opt-in to only having specific targets
        let mut targets: HashSet<_> = self
            .targets
            .as_ref()
            .map(|targets| targets.iter().map(String::as_str).collect())
            .unwrap_or_else(|| DEFAULT_TARGETS.iter().copied().collect());

        targets.remove(&default_target);
        BuildTargets {
            default_target,
            other_targets: targets,
        }
    }

    /// Return the arguments that should be passed to `cargo`.
    ///
    // TODO: maybe it shouldn't?
    /// This will always include `doc --lib --no-deps`.
    /// This will never include `--target`.
    ///
    /// Note that this does not necessarily reproduce the HTML _output_ of docs.rs exactly.
    /// For example, the links may point somewhere different than they would on docs.rs.
    /// However, rustdoc will see exactly the same code as it would on docs.rs, even counting `cfg`s.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut cargo_args: Vec<String> = vec!["doc".into(), "--lib".into(), "--no-deps".into()];

        if let Some(features) = &self.features {
            cargo_args.push("--features".into());
            cargo_args.push(features.join(" "));
        }

        if self.all_features {
            cargo_args.push("--all-features".into());
        }

        if self.no_default_features {
            cargo_args.push("--no-default-features".into());
        }

        cargo_args
    }

    /// Return the environment variables that should be set when building this crate.
    pub fn environment_variables(&self) -> HashMap<&'static str, String> {
        let joined = |v: &Option<Vec<_>>| v.as_ref()
            .map(|args| args.join(" "))
            .unwrap_or_default();

        let mut map = HashMap::new();
        map.insert("RUSTFLAGS", joined(&self.rustc_args));
        map.insert("RUSTDOCFLAGS", joined(&self.rustdoc_args));
        // For docs.rs detection from build scripts:
        // https://github.com/rust-lang/docs.rs/issues/147
        map.insert("DOCS_RS", "1".into());

        map
    }
}

impl std::str::FromStr for Metadata {
    type Err = toml::de::Error;

    /// Parse the given manifest as TOML.
    fn from_str(manifest: &str) -> Result<Metadata, Self::Err> {
        let mut metadata = Metadata::default();

        let manifest = manifest.parse::<Value>()?;

        fn fetch_manifest_tables<'a>(manifest: &'a Value) -> Option<&'a Map<String, Value>> {
            manifest
                .get("package")?
                .as_table()?
                .get("metadata")?
                .as_table()?
                .get("docs")?
                .as_table()?
                .get("rs")?
                .as_table()
        }

        if let Some(table) = fetch_manifest_tables(&manifest) {
            // TODO: all this `to_owned` is inefficient, this should use explicit matches instead.
            let collect_into_array =
                |f: &Vec<Value>| f.iter().map(|v| v.as_str().map(|v| v.to_owned())).collect();

            metadata.features = table
                .get("features")
                .and_then(|f| f.as_array())
                .and_then(collect_into_array);

            metadata.no_default_features = table
                .get("no-default-features")
                .and_then(|v| v.as_bool())
                .unwrap_or(metadata.no_default_features);

            metadata.all_features = table
                .get("all-features")
                .and_then(|v| v.as_bool())
                .unwrap_or(metadata.all_features);

            metadata.default_target = table
                .get("default-target")
                .and_then(|v| v.as_str())
                .map(|v| v.to_owned());

            metadata.targets = table
                .get("targets")
                .and_then(|f| f.as_array())
                .and_then(collect_into_array);

            metadata.rustc_args = table
                .get("rustc-args")
                .and_then(|f| f.as_array())
                .and_then(collect_into_array);

            metadata.rustdoc_args = table
                .get("rustdoc-args")
                .and_then(|f| f.as_array())
                .and_then(collect_into_array);
        }

        Ok(metadata)
    }
}

impl Default for Metadata {
    /// The metadata that is used if there is no `[package.metadata.docs.rs]` in `Cargo.toml`.
    fn default() -> Metadata {
        Metadata {
            features: None,
            all_features: false,
            no_default_features: false,
            default_target: None,
            rustc_args: None,
            rustdoc_args: None,
            targets: None,
        }
    }
}

#[cfg(test)]
mod test_parsing {
    use std::str::FromStr;
    use super::*;

    #[test]
    fn test_cratesfyi_metadata() {
        let manifest = r#"
            [package]
            name = "test"

            [package.metadata.docs.rs]
            features = [ "feature1", "feature2" ]
            all-features = true
            no-default-features = true
            default-target = "x86_64-unknown-linux-gnu"
            targets = [ "x86_64-apple-darwin", "x86_64-pc-windows-msvc" ]
            rustc-args = [ "--example-rustc-arg" ]
            rustdoc-args = [ "--example-rustdoc-arg" ]
        "#;

        let metadata = Metadata::from_str(manifest).unwrap();

        assert!(metadata.features.is_some());
        assert!(metadata.all_features);
        assert!(metadata.no_default_features);
        assert!(metadata.default_target.is_some());
        assert!(metadata.rustdoc_args.is_some());

        let features = metadata.features.unwrap();
        assert_eq!(features.len(), 2);
        assert_eq!(features[0], "feature1".to_owned());
        assert_eq!(features[1], "feature2".to_owned());

        assert_eq!(
            metadata.default_target.unwrap(),
            "x86_64-unknown-linux-gnu".to_owned()
        );

        let targets = metadata.targets.expect("should have explicit target");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0], "x86_64-apple-darwin");
        assert_eq!(targets[1], "x86_64-pc-windows-msvc");

        let rustc_args = metadata.rustc_args.unwrap();
        assert_eq!(rustc_args.len(), 1);
        assert_eq!(rustc_args[0], "--example-rustc-arg".to_owned());

        let rustdoc_args = metadata.rustdoc_args.unwrap();
        assert_eq!(rustdoc_args.len(), 1);
        assert_eq!(rustdoc_args[0], "--example-rustdoc-arg".to_owned());
    }

    #[test]
    fn test_no_targets() {
        // metadata section but no targets
        let manifest = r#"
            [package]
            name = "test"

            [package.metadata.docs.rs]
            features = [ "feature1", "feature2" ]
        "#;
        let metadata = Metadata::from_str(manifest).unwrap();
        assert!(metadata.targets.is_none());

        // no package.metadata.docs.rs section
        let metadata = Metadata::from_str(
            r#"
            [package]
            name = "test"
        "#,
        ).unwrap();
        assert!(metadata.targets.is_none());

        // targets explicitly set to empty array
        let metadata = Metadata::from_str(
            r#"
            [package.metadata.docs.rs]
            targets = []
        "#,
        ).unwrap();
        assert!(metadata.targets.unwrap().is_empty());
    }
}

#[cfg(test)]
mod test_targets {
    use super::*;

    #[test]
    fn test_select_targets() {
        use super::BuildTargets;

        let mut metadata = Metadata::default();

        // unchanged default_target, targets not specified
        let BuildTargets {
            default_target: default,
            other_targets: tier_one,
        } = metadata.targets();
        assert_eq!(default, HOST_TARGET);

        // should be equal to TARGETS \ {HOST_TARGET}
        for actual in &tier_one {
            assert!(DEFAULT_TARGETS.contains(actual));
        }

        for expected in DEFAULT_TARGETS {
            if *expected == HOST_TARGET {
                assert!(!tier_one.contains(&HOST_TARGET));
            } else {
                assert!(tier_one.contains(expected));
            }
        }

        // unchanged default_target, targets specified to be empty
        metadata.targets = Some(Vec::new());

        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, HOST_TARGET);
        assert!(others.is_empty());

        // unchanged default_target, targets non-empty
        metadata.targets = Some(vec![
            "i686-pc-windows-msvc".into(),
            "i686-apple-darwin".into(),
        ]);

        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, "i686-pc-windows-msvc");
        assert_eq!(others.len(), 1);
        assert!(others.contains(&"i686-apple-darwin"));

        // make sure that default_target is not built twice
        metadata.targets = Some(vec![HOST_TARGET.into()]);
        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, HOST_TARGET);
        assert!(others.is_empty());

        // make sure that duplicates are removed
        metadata.targets = Some(vec![
            "i686-pc-windows-msvc".into(),
            "i686-pc-windows-msvc".into(),
        ]);

        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, "i686-pc-windows-msvc");
        assert!(others.is_empty());

        // make sure that `default_target` always takes priority over `targets`
        metadata.default_target = Some("i686-apple-darwin".into());
        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, "i686-apple-darwin");
        assert_eq!(others.len(), 1);
        assert!(others.contains(&"i686-pc-windows-msvc"));

        // make sure that `default_target` takes priority over `HOST_TARGET`
        metadata.targets = Some(vec![]);
        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, "i686-apple-darwin");
        assert!(others.is_empty());

        // and if `targets` is unset, it should still be set to `TARGETS`
        metadata.targets = None;
        let BuildTargets {
            default_target: default,
            other_targets: others,
        } = metadata.targets();

        assert_eq!(default, "i686-apple-darwin");
        let tier_one_targets_no_default = DEFAULT_TARGETS
            .iter()
            .filter(|&&t| t != "i686-apple-darwin")
            .copied()
            .collect();

        assert_eq!(others, tier_one_targets_no_default);
    }
}

#[cfg(test)]
mod test_calculations {
    use super::*;

    fn default_cargo_args() -> Vec<String> {
        vec!["doc".into(), "--lib".into(), "--no-deps".into()]
    }

    #[test]
    fn test_defaults() {
        let metadata = Metadata::default();
        assert_eq!(metadata.cargo_args(), default_cargo_args());
        let env = metadata.environment_variables();
        assert_eq!(env.get("DOCS_RS").map(String::as_str), Some("1"));
        assert_eq!(env.get("RUSTDOCFLAGS").map(String::as_str), Some(""));
        assert_eq!(env.get("RUSTFLAGS").map(String::as_str), Some(""));
    }

    #[test]
    fn test_features() {
        // all features
        let metadata = Metadata {
            all_features: true,
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--all-features".into());
        assert_eq!(metadata.cargo_args(), expected_args);

        // no default features
        let metadata = Metadata {
            no_default_features: true,
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--no-default-features".into());
        assert_eq!(metadata.cargo_args(), expected_args);

        // allow passing both even though it's nonsense; cargo will give an error anyway
        let metadata = Metadata {
            all_features: true,
            no_default_features: true,
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--all-features".into());
        expected_args.push("--no-default-features".into());
        assert_eq!(metadata.cargo_args(), expected_args);

        // explicit empty vec
        let metadata = Metadata {
            features: Some(vec![]),
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--features".into());
        expected_args.push(String::new());
        assert_eq!(metadata.cargo_args(), expected_args);

        // one feature
        let metadata = Metadata {
            features: Some(vec!["some_feature".into()]),
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--features".into());
        expected_args.push("some_feature".into());
        assert_eq!(metadata.cargo_args(), expected_args);

        // multiple features
        let metadata = Metadata {
            features: Some(vec!["feature1".into(), "feature2".into()]),
            ..Metadata::default()
        };
        let mut expected_args = default_cargo_args();
        expected_args.push("--features".into());
        expected_args.push("feature1 feature2".into());
        assert_eq!(metadata.cargo_args(), expected_args);

        // rustdocflags
        let metadata = Metadata {
            rustdoc_args: Some(vec!["-Z".into(), "unstable-options".into(), "--static-root-path".into(), "/".into(), "--cap-lints".into(), "warn".into()]),
            ..Metadata::default()
        };
        assert_eq!(metadata.environment_variables().get("RUSTDOCFLAGS").map(String::as_str), Some("-Z unstable-options --static-root-path / --cap-lints warn"));

        // rustdocflags
        let metadata = Metadata {
            rustc_args: Some(vec!["-Z".into(), "unstable-options".into(), "--static-root-path".into(), "/".into(), "--cap-lints".into(), "warn".into()]),
            ..Metadata::default()
        };
        assert_eq!(metadata.environment_variables().get("RUSTFLAGS").map(String::as_str), Some("-Z unstable-options --static-root-path / --cap-lints warn"));
    }
}