use crate::{
    buffers::Acker, conditions, event::Metric, shutdown::ShutdownSignal, sinks, sources,
    transforms, Pipeline,
};
use async_trait::async_trait;
use component::ComponentDescription;
use indexmap::IndexMap; // IndexMap preserves insertion order, allowing us to output errors in the same order they are present in the file
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::{HashMap, HashSet};
use std::fs::DirBuilder;
use std::hash::Hash;
use std::net::SocketAddr;
use std::path::PathBuf;

pub mod api;
mod builder;
mod compiler;
pub mod component;
mod diff;
mod loading;
mod log_schema;
mod unit_test;
mod validation;
mod vars;
pub mod watcher;

pub use builder::ConfigBuilder;
pub use diff::ConfigDiff;
pub use loading::{load_from_paths, load_from_str, process_paths, CONFIG_PATHS};
pub use log_schema::{log_schema, LogSchema, LOG_SCHEMA};
pub use unit_test::build_unit_tests_main as build_unit_tests;
pub use validation::warnings;

#[derive(Debug, Default)]
pub struct Config {
    pub global: GlobalOptions,
    #[cfg(feature = "api")]
    pub api: api::Options,
    pub sources: IndexMap<String, Box<dyn SourceConfig>>,
    pub sinks: IndexMap<String, SinkOuter>,
    pub transforms: IndexMap<String, TransformOuter>,
    tests: Vec<TestDefinition>,
    expansions: IndexMap<String, Vec<String>>,
}

#[derive(Default, Debug, Deserialize, Serialize)]
pub struct GlobalOptions {
    #[serde(default = "default_data_dir")]
    pub data_dir: Option<PathBuf>,
    #[serde(
        skip_serializing_if = "crate::serde::skip_serializing_if_default",
        default
    )]
    pub log_schema: LogSchema,
}

pub fn default_data_dir() -> Option<PathBuf> {
    Some(PathBuf::from("/var/lib/vector/"))
}

#[derive(Debug, Snafu)]
pub enum DataDirError {
    #[snafu(display("data_dir option required, but not given here or globally"))]
    MissingDataDir,
    #[snafu(display("data_dir {:?} does not exist", data_dir))]
    DoesNotExist { data_dir: PathBuf },
    #[snafu(display("data_dir {:?} is not writable", data_dir))]
    NotWritable { data_dir: PathBuf },
    #[snafu(display(
        "Could not create subdirectory {:?} inside of data dir {:?}: {}",
        subdir,
        data_dir,
        source
    ))]
    CouldNotCreate {
        subdir: PathBuf,
        data_dir: PathBuf,
        source: std::io::Error,
    },
}

impl GlobalOptions {
    /// Resolve the `data_dir` option in either the global or local
    /// config, and validate that it exists and is writable.
    pub fn resolve_and_validate_data_dir(
        &self,
        local_data_dir: Option<&PathBuf>,
    ) -> crate::Result<PathBuf> {
        let data_dir = local_data_dir
            .or_else(|| self.data_dir.as_ref())
            .ok_or(DataDirError::MissingDataDir)
            .map_err(Box::new)?
            .to_path_buf();
        if !data_dir.exists() {
            return Err(DataDirError::DoesNotExist { data_dir }.into());
        }
        let readonly = std::fs::metadata(&data_dir)
            .map(|meta| meta.permissions().readonly())
            .unwrap_or(true);
        if readonly {
            return Err(DataDirError::NotWritable { data_dir }.into());
        }
        Ok(data_dir)
    }

    /// Resolve the `data_dir` option using
    /// `resolve_and_validate_data_dir` and then ensure a named
    /// subdirectory exists.
    pub fn resolve_and_make_data_subdir(
        &self,
        local: Option<&PathBuf>,
        subdir: &str,
    ) -> crate::Result<PathBuf> {
        let data_dir = self.resolve_and_validate_data_dir(local)?;

        let mut data_subdir = data_dir.clone();
        data_subdir.push(subdir);

        DirBuilder::new()
            .recursive(true)
            .create(&data_subdir)
            .with_context(|| CouldNotCreate { subdir, data_dir })?;
        Ok(data_subdir)
    }
}

#[derive(Debug, Clone, PartialEq, Copy)]
pub enum DataType {
    Any,
    Log,
    Metric,
}

pub trait GenerateConfig {
    fn generate_config() -> toml::Value;
}

#[macro_export]
macro_rules! impl_generate_config_from_default {
    ($type:ty) => {
        impl $crate::config::GenerateConfig for $type {
            fn generate_config() -> toml::Value {
                toml::Value::try_from(&Self::default()).unwrap()
            }
        }
    };
}

#[async_trait::async_trait]
#[async_trait]
#[typetag::serde(tag = "type")]
pub trait SourceConfig: core::fmt::Debug + Send + Sync {
    async fn build(
        &self,
        name: &str,
        globals: &GlobalOptions,
        shutdown: ShutdownSignal,
        out: Pipeline,
    ) -> crate::Result<sources::Source>;

    fn output_type(&self) -> DataType;

    fn source_type(&self) -> &'static str;

    /// Resources that the source is using.
    fn resources(&self) -> Vec<Resource> {
        Vec::new()
    }
}

pub type SourceDescription = ComponentDescription<Box<dyn SourceConfig>>;

inventory::collect!(SourceDescription);

#[derive(Deserialize, Serialize, Debug)]
pub struct SinkOuter {
    #[serde(default)]
    pub buffer: crate::buffers::BufferConfig,
    #[serde(default = "healthcheck_default")]
    pub healthcheck: bool,
    pub inputs: Vec<String>,
    #[serde(flatten)]
    pub inner: Box<dyn SinkConfig>,
}

#[async_trait]
#[typetag::serde(tag = "type")]
pub trait SinkConfig: core::fmt::Debug + Send + Sync {
    async fn build(
        &self,
        cx: SinkContext,
    ) -> crate::Result<(sinks::VectorSink, sinks::Healthcheck)>;

    fn input_type(&self) -> DataType;

    fn sink_type(&self) -> &'static str;

    /// Resources that the sink is using.
    fn resources(&self) -> Vec<Resource> {
        Vec::new()
    }
}

#[derive(Debug, Clone)]
pub struct SinkContext {
    pub(super) acker: Acker,
}

impl SinkContext {
    #[cfg(test)]
    pub fn new_test() -> Self {
        Self { acker: Acker::Null }
    }

    pub fn acker(&self) -> Acker {
        self.acker.clone()
    }
}

pub type SinkDescription = ComponentDescription<Box<dyn SinkConfig>>;

inventory::collect!(SinkDescription);

#[derive(Deserialize, Serialize, Debug)]
pub struct TransformOuter {
    pub inputs: Vec<String>,
    #[serde(flatten)]
    pub inner: Box<dyn TransformConfig>,
}

#[async_trait]
#[typetag::serde(tag = "type")]
pub trait TransformConfig: core::fmt::Debug + Send + Sync + dyn_clone::DynClone {
    async fn build(&self) -> crate::Result<transforms::Transform>;

    fn input_type(&self) -> DataType;

    fn output_type(&self) -> DataType;

    fn transform_type(&self) -> &'static str;

    /// Allows a transform configuration to expand itself into multiple "child"
    /// transformations to replace it. This allows a transform to act as a macro
    /// for various patterns.
    fn expand(&mut self) -> crate::Result<Option<IndexMap<String, Box<dyn TransformConfig>>>> {
        Ok(None)
    }
}

dyn_clone::clone_trait_object!(TransformConfig);

pub type TransformDescription = ComponentDescription<Box<dyn TransformConfig>>;

inventory::collect!(TransformDescription);

/// Unique thing, like port, of which only one owner can be.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Resource {
    Port(SocketAddr),
    SystemFdOffset(usize),
    Stdin,
}

impl Resource {
    /// From given components returns all that have a resource conflict with any other component.
    pub fn conflicts<K: Eq + Hash + Clone>(
        components: impl IntoIterator<Item = (K, Vec<Resource>)>,
    ) -> HashSet<K> {
        let mut resource_map = HashMap::<Resource, HashSet<K>>::new();
        let mut unspecified = Vec::new();

        // Find equality based conflicts
        for (key, resources) in components {
            for resource in resources {
                if let Resource::Port(address) = &resource {
                    if address.ip().is_unspecified() {
                        unspecified.push((key.clone(), address.port()));
                    }
                }

                resource_map
                    .entry(resource)
                    .or_default()
                    .insert(key.clone());
            }
        }

        // Port with unspecified address will bind to all network interfaces
        // so we have to check for all Port resources if they share the same
        // port.
        for (key, port) in unspecified {
            for (resource, components) in resource_map.iter_mut() {
                if let Resource::Port(address) = resource {
                    if address.port() == port {
                        components.insert(key.clone());
                    }
                }
            }
        }

        resource_map
            .into_iter()
            .filter(|(_, components)| components.len() > 1)
            .flat_map(|(_, components)| components)
            .collect()
    }
}

impl From<SocketAddr> for Resource {
    fn from(addr: SocketAddr) -> Self {
        Self::Port(addr)
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct TestDefinition {
    pub name: String,
    pub input: Option<TestInput>,
    #[serde(default)]
    pub inputs: Vec<TestInput>,
    #[serde(default)]
    pub outputs: Vec<TestOutput>,
    #[serde(default)]
    pub no_outputs_from: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(untagged)]
pub enum TestInputValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct TestInput {
    pub insert_at: String,
    #[serde(default = "default_test_input_type", rename = "type")]
    pub type_str: String,
    pub value: Option<String>,
    pub log_fields: Option<IndexMap<String, TestInputValue>>,
    pub metric: Option<Metric>,
}

fn default_test_input_type() -> String {
    "raw".to_string()
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct TestOutput {
    pub extract_from: String,
    pub conditions: Option<Vec<TestCondition>>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum TestCondition {
    Embedded(Box<dyn conditions::ConditionConfig>),
    NoTypeEmbedded(conditions::CheckFieldsConfig),
    String(String),
}

impl Config {
    pub fn builder() -> builder::ConfigBuilder {
        Default::default()
    }

    /// Expand a logical component name (i.e. from the config file) into the names of the
    /// components it was expanded to as part of the macro process. Does not check that the
    /// identifier is otherwise valid.
    pub fn get_inputs(&self, identifier: &str) -> Vec<String> {
        self.expansions
            .get(identifier)
            .cloned()
            .unwrap_or_else(|| vec![String::from(identifier)])
    }
}

fn healthcheck_default() -> bool {
    true
}

#[cfg(all(
    test,
    feature = "sources-file",
    feature = "sinks-console",
    feature = "transforms-json_parser"
))]
mod test {
    use super::{builder::ConfigBuilder, load_from_str};
    use std::path::PathBuf;

    #[test]
    fn default_data_dir() {
        let config = load_from_str(
            r#"
      [sources.in]
      type = "file"
      include = ["/var/log/messages"]

      [sinks.out]
      type = "console"
      inputs = ["in"]
      encoding = "json"
      "#,
        )
        .unwrap();

        assert_eq!(
            Some(PathBuf::from("/var/lib/vector")),
            config.global.data_dir
        )
    }

    #[test]
    fn default_schema() {
        let config = load_from_str(
            r#"
      [sources.in]
      type = "file"
      include = ["/var/log/messages"]

      [sinks.out]
      type = "console"
      inputs = ["in"]
      encoding = "json"
      "#,
        )
        .unwrap();

        assert_eq!("host", config.global.log_schema.host_key().to_string());
        assert_eq!(
            "message",
            config.global.log_schema.message_key().to_string()
        );
        assert_eq!(
            "timestamp",
            config.global.log_schema.timestamp_key().to_string()
        );
    }

    #[test]
    fn custom_schema() {
        let config = load_from_str(
            r#"
      [log_schema]
      host_key = "this"
      message_key = "that"
      timestamp_key = "then"

      [sources.in]
      type = "file"
      include = ["/var/log/messages"]

      [sinks.out]
      type = "console"
      inputs = ["in"]
      encoding = "json"
      "#,
        )
        .unwrap();

        assert_eq!("this", config.global.log_schema.host_key().to_string());
        assert_eq!("that", config.global.log_schema.message_key().to_string());
        assert_eq!("then", config.global.log_schema.timestamp_key().to_string());
    }

    #[test]
    fn config_append() {
        let mut config: ConfigBuilder = toml::from_str(
            r#"
      [sources.in]
      type = "file"
      include = ["/var/log/messages"]

      [sinks.out]
      type = "console"
      inputs = ["in"]
      encoding = "json"
      "#,
        )
        .unwrap();

        assert_eq!(
            config.append(
                toml::from_str(
                    r#"
        data_dir = "/foobar"

        [transforms.foo]
        type = "json_parser"
        inputs = [ "in" ]

        [[tests]]
        name = "check_simple_log"
        [tests.input]
        insert_at = "foo"
        type = "raw"
        value = "2019-11-28T12:00:00+00:00 info Sorry, I'm busy this week Cecil"
        [[tests.outputs]]
        extract_from = "foo"
        [[tests.outputs.conditions]]
        type = "check_fields"
        "message.equals" = "Sorry, I'm busy this week Cecil"
            "#,
                )
                .unwrap()
            ),
            Ok(())
        );

        assert_eq!(Some(PathBuf::from("/foobar")), config.global.data_dir);
        assert!(config.sources.contains_key("in"));
        assert!(config.sinks.contains_key("out"));
        assert!(config.transforms.contains_key("foo"));
        assert_eq!(config.tests.len(), 1);
    }

    #[test]
    fn config_append_collisions() {
        let mut config: ConfigBuilder = toml::from_str(
            r#"
      [sources.in]
      type = "file"
      include = ["/var/log/messages"]

      [sinks.out]
      type = "console"
      inputs = ["in"]
      encoding = "json"
      "#,
        )
        .unwrap();

        assert_eq!(
            config.append(
                toml::from_str(
                    r#"
        [sources.in]
        type = "file"
        include = ["/var/log/messages"]

        [transforms.foo]
        type = "json_parser"
        inputs = [ "in" ]

        [sinks.out]
        type = "console"
        inputs = ["in"]
        encoding = "json"
            "#,
                )
                .unwrap()
            ),
            Err(vec![
                "duplicate source name found: in".into(),
                "duplicate sink name found: out".into(),
            ])
        );
    }
}

#[cfg(all(test, feature = "sources-stdin", feature = "sinks-console"))]
mod resource_tests {
    use super::{load_from_str, Resource};
    use std::collections::HashSet;
    use std::net::{Ipv4Addr, SocketAddr};

    fn localhost(port: u16) -> Resource {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port).into()
    }

    #[test]
    fn valid() {
        let components = vec![
            ("sink_0", vec![localhost(0)]),
            ("sink_1", vec![localhost(1)]),
            ("sink_2", vec![localhost(2)]),
        ];
        let conflicting = Resource::conflicts(components);
        assert_eq!(conflicting, HashSet::new());
    }

    #[test]
    fn conflicting_pair() {
        let components = vec![
            ("sink_0", vec![localhost(0)]),
            ("sink_1", vec![localhost(2)]),
            ("sink_2", vec![localhost(2)]),
        ];
        let conflicting = Resource::conflicts(components);
        assert_eq!(conflicting, vec!["sink_1", "sink_2"].into_iter().collect());
    }

    #[test]
    fn conflicting_multi() {
        let components = vec![
            ("sink_0", vec![localhost(0)]),
            ("sink_1", vec![localhost(2), localhost(0)]),
            ("sink_2", vec![localhost(2)]),
        ];
        let conflicting = Resource::conflicts(components);
        assert_eq!(
            conflicting,
            vec!["sink_0", "sink_1", "sink_2"].into_iter().collect()
        );
    }

    #[test]
    fn different_network_interface() {
        let components = vec![
            ("sink_0", vec![localhost(0)]),
            (
                "sink_1",
                vec![SocketAddr::new(Ipv4Addr::new(127, 0, 0, 2).into(), 0).into()],
            ),
        ];
        let conflicting = Resource::conflicts(components);
        assert_eq!(conflicting, HashSet::new());
    }

    #[test]
    fn unspecified_network_interface() {
        let components = vec![
            ("sink_0", vec![localhost(0)]),
            (
                "sink_1",
                vec![SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0).into()],
            ),
        ];
        let conflicting = Resource::conflicts(components);
        assert_eq!(conflicting, vec!["sink_0", "sink_1"].into_iter().collect());
    }

    #[test]
    fn config_conflict_detected() {
        assert!(load_from_str(
            r#"
        [sources.in0]
        type = "stdin"

        [sources.in1]
        type = "stdin"

        [sinks.out]
        type = "console"
        inputs = ["in0","in1"]
        encoding = "json"
        "#
        )
        .is_err());
    }
}
