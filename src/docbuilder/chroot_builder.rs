
use super::DocBuilder;
use super::crates::crates_from_path;
use DocBuilderError;
use utils::{get_package, source_path, copy_dir, copy_doc_dir, update_sources};
use db::{connect_db, add_package_into_database, add_build_into_database, add_path_into_database};
use cargo::core::Package;
use std::process::{Command, Output};
use std::path::PathBuf;
use postgres::Connection;

use regex::Regex;


type CommandResult = Result<String, String>;

#[derive(Debug)]
pub struct ChrootBuilderResult {
    pub output: String,
    pub build_success: bool,
    pub have_doc: bool,
    pub have_examples: bool,
    pub rustc_version: String,
    pub cratesfyi_version: String,
}


impl DocBuilder {
    /// Builds every package documentation in chroot environment
    pub fn build_world(&mut self) -> Result<(), DocBuilderError> {
        try!(update_sources());

        let mut count = 0;

        crates(self.options.crates_io_index_path.clone(), |name, version| {
            match self.build_package(name, version) {
                Ok(status) => {
                    count += 1;
                    if status && count % 10 == 0 {
                        let _ = self.save_cache();
                    }
                }
                Err(err) => warn!("Failed to build package {}-{}: {}", name, version, err),
            }
            self.cache.insert(format!("{}-{}", name, version));
        })
    }


    /// Builds package documentation in chroot environment and adds into cratesfyi database
    pub fn build_package(&mut self, name: &str, version: &str) -> Result<bool, DocBuilderError> {
        info!("Building package {}-{}", name, version);

        // Database connection
        let conn = try!(connect_db());

        // Skip crates according to options
        if (self.options.skip_if_log_exists &&
            self.cache.contains(&format!("{}-{}", name, version)[..])) ||
           (self.options.skip_if_exists &&
            self.db_cache.contains(&format!("{}-{}", name, version)[..])) {
            info!("Skipping");
            return Ok(false);
        }


        // get_package (and cargo) is using semver, add '=' in front of version.
        let pkg = try!(get_package(name, Some(&format!("={}", version)[..])));
        let res = self.build_package_in_chroot(&pkg);

        // copy sources and documentation
        try!(self.add_sources_into_database(&conn, &pkg));
        if res.have_doc {
            try!(self.copy_documentation(&pkg, &res.rustc_version));
            try!(self.add_documentation_into_database(&conn, &pkg));
        }

        let release_id = try!(add_package_into_database(&conn, &pkg, &res));
        try!(add_build_into_database(&conn, &release_id, &res));

        // remove documentation, source and build directory after we are done
        try!(self.clean(&pkg));

        // add package into build cache
        self.cache.insert(format!("{}-{}", name, version));

        Ok(res.build_success)
    }


    /// Builds documentation of a package with cratesfyi in chroot environment
    fn build_package_in_chroot(&self, package: &Package) -> ChrootBuilderResult {
        debug!("Building package in chroot");
        let (rustc_version, cratesfyi_version) = self.get_versions();
        let cmd = format!("cratesfyi doc {} ={}",
                          package.manifest().name(),
                          package.manifest().version());
        match self.chroot_command(cmd) {
            Ok(o) => {
                ChrootBuilderResult {
                    output: o,
                    build_success: true,
                    have_doc: self.have_documentation(&package),
                    have_examples: self.have_examples(&package),
                    rustc_version: rustc_version,
                    cratesfyi_version: cratesfyi_version,
                }
            }
            Err(e) => {
                ChrootBuilderResult {
                    output: e,
                    build_success: false,
                    have_doc: false,
                    have_examples: self.have_examples(&package),
                    rustc_version: rustc_version,
                    cratesfyi_version: cratesfyi_version,
                }
            }
        }
    }


    /// Copies source files of a package into source_path
    #[allow(dead_code)]  // I've been using this function before storing files in database
    fn copy_sources(&self, package: &Package) -> Result<(), DocBuilderError> {
        debug!("Copying sources");
        let destination = PathBuf::from(&self.options.sources_path).join(format!("{}/{}",
                          package.manifest().name(),
                          package.manifest().version()));
        // unwrap is safe here, this function will be always called after get_package
        match copy_dir(source_path(&package).unwrap(), &destination) {
            Ok(_) => Ok(()),
            Err(e) => Err(DocBuilderError::Io(e)),
        }
    }


    /// Copies documentation to destination directory
    fn copy_documentation(&self,
                          package: &Package,
                          rustc_version: &str)
                          -> Result<(), DocBuilderError> {
        debug!("Copying codumentation");
        let crate_doc_path = PathBuf::from(&self.options.chroot_path)
                                 .join("home")
                                 .join(&self.options.chroot_user)
                                 .join(canonical_name(&package));
        let destination = PathBuf::from(&self.options.destination).join(format!("{}/{}",
                          package.manifest().name(),
                          package.manifest().version()));
        copy_doc_dir(crate_doc_path,
                     destination,
                     parse_rustc_version(rustc_version).trim())
            .map_err(DocBuilderError::Io)
    }


    /// Removes build directory of a package in chroot
    fn remove_build_dir(&self, package: &Package) -> Result<(), DocBuilderError> {
        debug!("Removing build directory");
        let _ = self.chroot_command(format!("rm -rf {}", canonical_name(&package)));
        Ok(())
    }


    /// Remove documentation, build directory and sources directory of a package
    fn clean(&self, package: &Package) -> Result<(), DocBuilderError> {
        use std::fs::remove_dir_all;
        try!(self.remove_build_dir(&package));
        let documentation_path = PathBuf::from(&self.options.destination).join(format!("{}/{}",
                          package.manifest().name(),
                          package.manifest().version()));
        let source_path = PathBuf::from(&self.options.sources_path).join(format!("{}/{}",
                          package.manifest().name(),
                          package.manifest().version()));
        try!(remove_dir_all(documentation_path));
        try!(remove_dir_all(source_path));
        Ok(())
    }


    /// Runs a command in a chroot environment
    fn chroot_command<T: AsRef<str>>(&self, cmd: T) -> CommandResult {
        command_result(Command::new("sudo")
                           .arg("lxc-attach")
                           .arg("-n")
                           .arg(&self.options.container_name)
                           .arg("--")
                           .arg("su")
                           .arg("-")
                           .arg(&self.options.chroot_user)
                           .arg("-c")
                           .arg(cmd.as_ref())
                           .output()
                           .unwrap())
    }


    /// Checks a package build directory to determine if package have docs
    ///
    /// This function is checking first target in targets to see if documentation exists for a
    /// crate. Package must be successfully built in chroot environment first.
    fn have_documentation(&self, package: &Package) -> bool {
        let crate_doc_path = PathBuf::from(&self.options.chroot_path)
                                 .join("home")
                                 .join(&self.options.chroot_user)
                                 .join(canonical_name(&package))
                                 .join("doc")
                                 .join(package.targets()[0].name().to_string());
        crate_doc_path.exists()
    }


    /// Checks if package have examples
    fn have_examples(&self, package: &Package) -> bool {
        let path = source_path(&package).unwrap().join("examples");
        path.exists() && path.is_dir()
    }


    /// Gets rustc and cratesfyi version from chroot environment
    fn get_versions(&self) -> (String, String) {
        // It is safe to use expect here
        // chroot environment must always have rustc and cratesfyi installed
        (String::from(self.chroot_command("rustc --version")
                          .expect("Failed to get rustc version")
                          .trim()),
         String::from(self.chroot_command("cratesfyi --version")
                          .expect("Failed to get cratesfyi version")
                          .trim()))
    }


    /// Adds sources into database
    fn add_sources_into_database(&self,
                                 conn: &Connection,
                                 package: &Package)
                                 -> Result<(), DocBuilderError> {
        debug!("Adding sources into database");
        let prefix = format!("sources/{}/{}",
                             package.manifest().name(),
                             package.manifest().version());
        add_path_into_database(conn, &prefix, source_path(&package).unwrap())
    }


    /// Adds documentations into database
    fn add_documentation_into_database(&self,
                                       conn: &Connection,
                                       package: &Package)
                                       -> Result<(), DocBuilderError> {
        debug!("Adding documentation into database");
        let prefix = format!("rustdoc/{}/{}",
                             package.manifest().name(),
                             package.manifest().version());
        let crate_doc_path = PathBuf::from(&self.options.chroot_path)
                                 .join("home")
                                 .join(&self.options.chroot_user)
                                 .join(canonical_name(&package));
        add_path_into_database(conn, &prefix, crate_doc_path)
    }
}


/// Simple function to capture command output
fn command_result(output: Output) -> CommandResult {
    let mut command_out = String::from_utf8_lossy(&output.stdout).into_owned();
    command_out.push_str(&String::from_utf8_lossy(&output.stderr).into_owned()[..]);
    match output.status.success() {
        true => Ok(command_out),
        false => Err(command_out),
    }
}



/// Returns canonical name of a package.
///
/// It's just package-version. All directory structure used in cratesfyi is
/// following this naming scheme.
fn canonical_name(package: &Package) -> String {
    format!("{}-{}",
            package.manifest().name(),
            package.manifest().version())
}


/// Parses rustc commit hash from rustc version string
fn parse_rustc_version<S: AsRef<str>>(version: S) -> String {
    let version_regex = Regex::new(r" ([\w-.]+) \((\w+) (\d+)-(\d+)-(\d+)\)").unwrap();
    let captures = version_regex.captures(version.as_ref()).expect("Failed to parse rustc version");

    format!("{}{}{}-{}-{}",
            captures.at(3).unwrap(),
            captures.at(4).unwrap(),
            captures.at(5).unwrap(),
            captures.at(1).unwrap(),
            captures.at(2).unwrap())
}


/// Runs `func` with the all crates from crates-io.index repository path.
///
/// First argument of func is the name of crate and
/// second argument is the version of crate. Func will be run for every crate.
fn crates<F>(path: PathBuf, mut func: F) -> Result<(), DocBuilderError>
    where F: FnMut(&str, &str) -> ()
{
    crates_from_path(&path, &mut func)
}


#[cfg(test)]
mod test {
    extern crate env_logger;
    use super::parse_rustc_version;
    use std::path::PathBuf;
    use {DocBuilder, DocBuilderOptions};

    #[test]
    #[ignore]
    fn test_build_world() {
        let _ = env_logger::init();
        let options = DocBuilderOptions::from_prefix(PathBuf::from("../cratesfyi-prefix"));
        let mut docbuilder = DocBuilder::new(options);
        // This test is building WHOLE WORLD and may take forever
        assert!(docbuilder.build_world().is_ok());
    }

    #[test]
    #[ignore]
    fn test_build_package() {
        let _ = env_logger::init();
        let options = DocBuilderOptions::from_prefix(PathBuf::from("../cratesfyi-prefix"));
        let mut docbuilder = DocBuilder::new(options);
        let res = docbuilder.build_package("rand", "0.3.14");
        assert!(res.is_ok());
    }

    #[test]
    fn test_parse_rustc_version() {
        assert_eq!(parse_rustc_version("rustc 1.10.0-nightly (57ef01513 2016-05-23)"),
                   "20160523-1.10.0-nightly-57ef01513");
        assert_eq!(parse_rustc_version("cratesfyi 0.2.0 (ba9ae23 2016-05-26)"),
                   "20160526-0.2.0-ba9ae23");
    }
}
