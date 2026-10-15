//! Fonti esterne di superzed (GitHub, HyperDX/ClickHouse, Zabbix): connessioni globali,
//! segreti nel Portachiavi, tunnel SSH e lettura degli elementi. Non dipende dal canvas.
pub mod clickhouse;
pub mod connections;
pub mod forms;
pub mod github;
pub mod gmail;
pub mod item;
pub mod runner;
pub mod secrets;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tunnel;
pub mod zabbix;

pub use clickhouse::{ChTransport, ClickhouseClient, HyperdxSource, LogQuery};
pub use connections::{
    ClickhouseConfig, ClickhouseTransport, Connection, ConnectionConfig, ConnectionKind, ConnectionsFile, Forward,
    GithubConfig, HyperdxConfig, TunnelConfig, ZabbixConfig, connections_path, parse_connections, read_connections,
    safe_host, safe_ident, secret_url, unique_id, validate, write_connections,
};
pub use forms::{FormField, connection_from_fields, fields_of, form_fields, format_forwards, insecure_http_warning, parse_forwards};
pub use github::GithubSource;
pub use gmail::{Draft, DraftContent, GmailClient, MailMessage, MailThread, ThreadSummary, project_query};
pub use item::{
    SOURCES_VIEW, choice_label,
    CONTEXT_MAX_CHARS, DATA_END, DATA_START, Filter, FilterField, FilterKind, Item, Severity, Source, SourceKind, chat_prompt,
    context_block, filter_value, parse_window, sanitize_context, validate_filter,
};
pub use runner::{ProcessRunner, RunError, RunOutput, Runner, augmented_path};
pub use secrets::{KeychainSecrets, MemorySecrets, SecretStore};
pub use store::{ConnectionStore, SourceDeps, filter_schema_for, init, init_test};
pub use tunnel::{SshTunnelCommand, TunnelCommand, TunnelState, TunnelSupervisor};
pub use zabbix::ZabbixSource;
