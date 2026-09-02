use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TOKEN_ENV: &str = "ZLS_JIRA_TOKEN";
const KEYRING_SERVICE: &str = "zls";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JiraConfig {
    pub site: String,
    pub email: String,
    pub cloud_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JiraUser {
    pub account_id: String,
    pub display_name: String,
    pub email_address: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueSummary {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
    pub assignee: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueLink {
    pub relationship: String,
    pub key: String,
    pub summary: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JiraComment {
    pub id: String,
    pub author: String,
    pub created: String,
    pub updated: String,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueCard {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub description: String,
    pub subtasks: Vec<IssueSummary>,
    pub links: Vec<IssueLink>,
    pub comments: Vec<JiraComment>,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub reporter: Option<String>,
    #[serde(default)]
    pub parent: Option<IssueSummary>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub sprints: Vec<String>,
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub fix_versions: Vec<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
}

#[derive(Clone)]
pub struct JiraClient {
    http: Client,
    config: JiraConfig,
    token: String,
    cache_dir: PathBuf,
}

impl std::fmt::Debug for JiraClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JiraClient")
            .field("config", &self.config)
            .field("cache_dir", &self.cache_dir)
            .finish_non_exhaustive()
    }
}

impl JiraConfig {
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read Jira config at {}", path.display()))?;
        let root: ConfigFile = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        let mut config = root
            .jira
            .ok_or_else(|| anyhow!("missing [jira] section in {}", path.display()))?;
        config.site = normalize_site(&config.site)?;
        if config.email.trim().is_empty() || config.cloud_id.trim().is_empty() {
            bail!("Jira email and cloud_id must not be empty");
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let mut root = if path.exists() {
            toml::from_str::<toml::Value>(&fs::read_to_string(path)?)
                .with_context(|| format!("invalid TOML in {}", path.display()))?
        } else {
            toml::Value::Table(Default::default())
        };
        let table = root
            .as_table_mut()
            .ok_or_else(|| anyhow!("{} must contain a TOML table", path.display()))?;
        table.insert("jira".to_owned(), toml::Value::try_from(self)?);
        fs::write(path, toml::to_string_pretty(&root)?)
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

#[derive(Deserialize)]
struct ConfigFile {
    jira: Option<JiraConfig>,
}

impl JiraClient {
    pub fn from_config() -> Result<Self> {
        let config = JiraConfig::load()?;
        let token = load_token(&config)?;
        Self::new(config, token)
    }

    pub fn new(config: JiraConfig, token: String) -> Result<Self> {
        if token.trim().is_empty() {
            bail!("Jira API token must not be empty");
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("zls/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create Jira HTTP client")?;
        Ok(Self {
            http,
            config,
            token,
            cache_dir: cache_dir()?,
        })
    }

    pub fn config(&self) -> &JiraConfig {
        &self.config
    }

    pub fn current_user(&self) -> Result<JiraUser> {
        let response: ApiUser = self
            .authenticated(self.http.get(self.api_url("/rest/api/3/myself")))
            .send()
            .context("failed to contact Jira")?
            .error_for_status()
            .context("Jira authentication failed")?
            .json()
            .context("invalid current-user response from Jira")?;
        Ok(response.into())
    }

    pub fn test_auth(&self) -> Result<JiraUser> {
        self.current_user()
    }

    pub fn search_issues(&self, query: &str) -> Result<Vec<IssueSummary>> {
        let jql = search_jql(query, self.config.project.as_deref());
        let payload = json!({
            "jql": jql,
            "maxResults": 50,
            "fields": ["summary", "status", "priority", "assignee"]
        });
        let response: SearchResponse = self
            .authenticated(
                self.http
                    .post(self.api_url("/rest/api/3/search/jql"))
                    .json(&payload),
            )
            .send()
            .context("failed to search Jira")?
            .error_for_status()
            .context("Jira issue search failed")?
            .json()
            .context("invalid issue-search response from Jira")?;
        Ok(response
            .issues
            .into_iter()
            .map(ApiIssue::into_summary)
            .collect())
    }

    pub fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let response: ProjectSearchResponse = self
            .authenticated(
                self.http
                    .get(self.api_url("/rest/api/3/project/search"))
                    .query(&[("maxResults", "100"), ("orderBy", "name")]),
            )
            .send()
            .context("failed to list Jira projects")?
            .error_for_status()
            .context("Jira project search failed")?
            .json()
            .context("invalid Jira project-search response")?;
        Ok(response
            .values
            .into_iter()
            .map(|project| ProjectSummary {
                key: project.key,
                name: project.name,
            })
            .collect())
    }

    pub fn set_project(&mut self, key: &str) -> Result<()> {
        let key = validate_project_key(key)?.to_owned();
        self.config.project = Some(key);
        self.config.save()
    }

    pub fn issue_card(&self, key: &str) -> Result<IssueCard> {
        let key = validate_issue_key(key)?;
        match self.fetch_issue_card(key) {
            Ok(card) => {
                let _ = self.write_cache(&card);
                Ok(card)
            }
            Err(network_error) => {
                let mut cached = self.read_cache(key).with_context(|| {
                    format!("failed to fetch {key} and no cached issue card is available")
                })?;
                cached.stale = true;
                cached.stale_reason = Some(format!("Jira request failed: {network_error:#}"));
                Ok(cached)
            }
        }
    }

    pub fn post_comment(&self, key: &str, text: &str) -> Result<JiraComment> {
        let key = validate_issue_key(key)?;
        if text.is_empty() {
            bail!("comment must not be empty");
        }
        let payload = comment_payload(text);
        let response: ApiComment = self
            .authenticated(
                self.http
                    .post(self.api_url(&format!("/rest/api/3/issue/{key}/comment")))
                    .json(&payload),
            )
            .send()
            .context("failed to post Jira comment")?
            .error_for_status()
            .context("Jira rejected the comment")?
            .json()
            .context("invalid comment response from Jira")?;
        Ok(response.into_comment())
    }

    pub fn issue_comments(&self, key: &str, limit: usize) -> Result<Vec<JiraComment>> {
        let key = validate_issue_key(key)?;
        let limit = limit.clamp(1, 100);
        let response: CommentsResponse = self
            .authenticated(self.http.get(self.api_url(&format!(
                "/rest/api/3/issue/{key}/comment?maxResults={limit}&orderBy=-created"
            ))))
            .send()
            .context("failed to fetch Jira comments")?
            .error_for_status()
            .context("Jira comment fetch failed")?
            .json()
            .context("invalid comments response from Jira")?;
        Ok(response
            .comments
            .into_iter()
            .take(limit)
            .map(ApiComment::into_comment)
            .collect())
    }

    pub fn transitions(&self, issue_key: &str) -> Result<Vec<Transition>> {
        let issue_key = validate_issue_key(issue_key)?;
        let response: TransitionsResponse = self
            .authenticated(
                self.http
                    .get(self.api_url(&format!("/rest/api/3/issue/{issue_key}/transitions"))),
            )
            .send()
            .context("failed to fetch Jira transitions")?
            .error_for_status()
            .context("Jira transition fetch failed")?
            .json()
            .context("invalid transitions response from Jira")?;
        Ok(response
            .transitions
            .into_iter()
            .map(ApiTransition::into_transition)
            .collect())
    }

    pub fn transition_issue(&self, issue_key: &str, transition_id: &str) -> Result<()> {
        let issue_key = validate_issue_key(issue_key)?;
        let transition_id = validate_transition_id(transition_id)?;
        self.authenticated(
            self.http
                .post(self.api_url(&format!("/rest/api/3/issue/{issue_key}/transitions")))
                .json(&transition_payload(transition_id)),
        )
        .send()
        .context("failed to transition Jira issue")?
        .error_for_status()
        .context("Jira rejected the transition")?;
        Ok(())
    }

    fn fetch_issue_card(&self, key: &str) -> Result<IssueCard> {
        let issue: ApiIssue = self
            .authenticated(
                self.http
                    .get(self.api_url(&format!("/rest/api/3/issue/{key}")))
                    .query(&[("fields", "*all"), ("expand", "names")]),
            )
            .send()
            .context("failed to fetch Jira issue")?
            .error_for_status()
            .context("Jira issue fetch failed")?
            .json()
            .context("invalid issue response from Jira")?;
        Ok(issue.into_card(self.issue_comments(key, 5)?))
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .basic_auth(&self.config.email, Some(&self.token))
            .header("Accept", "application/json")
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "https://api.atlassian.com/ex/jira/{}{}",
            self.config.cloud_id, path
        )
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{}.json", key.to_ascii_uppercase()))
    }

    fn write_cache(&self, card: &IssueCard) -> Result<()> {
        fs::create_dir_all(&self.cache_dir)
            .with_context(|| format!("failed to create {}", self.cache_dir.display()))?;
        let path = self.cache_path(&card.key);
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(card)?)?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("failed to update Jira cache {}", path.display()))
    }

    fn read_cache(&self, key: &str) -> Result<IssueCard> {
        let path = self.cache_path(key);
        let contents = fs::read(&path)
            .with_context(|| format!("failed to read Jira cache {}", path.display()))?;
        serde_json::from_slice(&contents)
            .with_context(|| format!("invalid Jira cache {}", path.display()))
    }
}

pub fn interactive_auth_setup() -> Result<JiraClient> {
    let mut site = String::new();
    let mut email = String::new();
    print!("Jira site (for example, company.atlassian.net): ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut site)?;
    print!("Jira account email: ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut email)?;
    let token = rpassword::prompt_password("Scoped Jira API token: ")?;

    let site = normalize_site(site.trim())?;
    let email = email.trim().to_owned();
    if email.is_empty() || token.is_empty() {
        bail!("Jira email and token must not be empty");
    }
    let cloud_id = discover_cloud_id(&site)?;
    let project = JiraConfig::load().ok().and_then(|existing| {
        (existing.site == site && existing.email == email)
            .then_some(existing.project)
            .flatten()
    });
    let config = JiraConfig {
        site,
        email,
        cloud_id,
        project,
    };
    let client = JiraClient::new(config.clone(), token.clone())?;
    client.test_auth()?;
    config.save()?;
    if let Err(error) = store_token(&config, &token) {
        eprintln!(
            "warning: Jira authentication succeeded, but the token was not persisted: {error:#}\n\
             install secret-tool and run auth again, or ensure ZLS_JIRA_TOKEN is available to the process starting zls\n\
             note: an existing tmux server does not automatically inherit variables exported later"
        );
    }
    Ok(client)
}

pub fn discover_cloud_id(site: &str) -> Result<String> {
    let site = normalize_site(site)?;
    let response: TenantInfo = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(format!("{site}/_edge/tenant_info"))
        .send()
        .context("failed to discover Jira Cloud tenant")?
        .error_for_status()
        .context("Jira tenant discovery failed")?
        .json()
        .context("invalid Jira tenant discovery response")?;
    if response.cloud_id.trim().is_empty() {
        bail!("Jira tenant discovery returned an empty cloud ID");
    }
    Ok(response.cloud_id)
}

pub fn render_adf(document: &Value) -> String {
    let mut output = String::new();
    render_node(document, &mut output, 0);
    while output.ends_with('\n') {
        output.pop();
    }
    output
}

pub fn format_jira_datetime(input: &str) -> String {
    DateTime::parse_from_rfc3339(input)
        .or_else(|_| DateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .map(|datetime| {
            datetime
                .with_timezone(&Local)
                .format("%b %-d, %Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| input.to_owned())
}

pub fn config_path() -> Result<PathBuf> {
    xdg_path("XDG_CONFIG_HOME", ".config").map(|path| path.join("zls/config.toml"))
}

pub fn cache_dir() -> Result<PathBuf> {
    xdg_path("XDG_CACHE_HOME", ".cache").map(|path| path.join("zls/jira"))
}

fn xdg_path(variable: &str, home_suffix: &str) -> Result<PathBuf> {
    if let Some(path) = env::var_os(variable).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(home_suffix))
        .ok_or_else(|| anyhow!("neither {variable} nor HOME is set"))
}

fn normalize_site(site: &str) -> Result<String> {
    let site = site.trim().trim_end_matches('/');
    let url = if site.starts_with("https://") {
        site.to_owned()
    } else if site.contains("://") {
        bail!("Jira site must use HTTPS");
    } else {
        format!("https://{site}")
    };
    let parsed = reqwest::Url::parse(&url).context("invalid Jira site URL")?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        bail!("Jira site must be a valid HTTPS URL");
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("Jira site must not include a path, query, or fragment");
    }
    Ok(url)
}

fn load_token(config: &JiraConfig) -> Result<String> {
    match lookup_keyring_token(config) {
        Ok(token) if !token.is_empty() => Ok(token),
        Ok(_) | Err(_) => env::var(TOKEN_ENV).with_context(|| {
            format!("no Jira token in the Linux keyring or {TOKEN_ENV} environment variable")
        }),
    }
}

fn lookup_keyring_token(config: &JiraConfig) -> Result<String> {
    let output = Command::new("secret-tool")
        .args([
            "lookup",
            "service",
            KEYRING_SERVICE,
            "jira-site",
            &config.site,
            "email",
            &config.email,
        ])
        .stderr(Stdio::null())
        .output()
        .context("failed to invoke secret-tool")?;
    if !output.status.success() {
        bail!("secret-tool could not find the Jira token");
    }
    String::from_utf8(output.stdout)
        .context("secret-tool returned non-UTF-8 data")
        .map(|token| token.trim_end().to_owned())
}

fn store_token(config: &JiraConfig, token: &str) -> Result<()> {
    let mut child = Command::new("secret-tool")
        .args([
            "store",
            "--label",
            "zls Jira Cloud API token",
            "service",
            KEYRING_SERVICE,
            "jira-site",
            &config.site,
            "email",
            &config.email,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("failed to invoke secret-tool; install libsecret tools or use ZLS_JIRA_TOKEN")?;
    child
        .stdin
        .take()
        .context("failed to open secret-tool stdin")?
        .write_all(token.as_bytes())?;
    let status = child.wait().context("failed to wait for secret-tool")?;
    if !status.success() {
        bail!("secret-tool failed to store the Jira token");
    }
    Ok(())
}

fn validate_issue_key(key: &str) -> Result<&str> {
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        bail!("invalid Jira issue key");
    }
    Ok(key)
}

fn validate_transition_id(id: &str) -> Result<&str> {
    let id = id.trim();
    if id.is_empty() || !id.chars().all(|character| character.is_ascii_digit()) {
        bail!("invalid Jira transition ID");
    }
    Ok(id)
}

fn escape_jql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn search_jql(query: &str, project: Option<&str>) -> String {
    let query = query.trim();
    let scope = project.map(|key| format!("project = \"{}\" AND ", escape_jql_string(key)));
    let filter = if query.is_empty() {
        "assignee = currentUser() AND resolution = Unresolved".to_owned()
    } else {
        let escaped = escape_jql_string(query);
        let looks_like_key = query.split_once('-').is_some_and(|(project, number)| {
            !project.is_empty()
                && project
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
                && !number.is_empty()
                && number.chars().all(|character| character.is_ascii_digit())
        });
        if looks_like_key {
            format!("(key = \"{escaped}\" OR text ~ \"{escaped}*\")")
        } else {
            format!("text ~ \"{escaped}*\"")
        }
    };
    format!(
        "{}{filter} ORDER BY updated DESC",
        scope.unwrap_or_default()
    )
}

fn validate_project_key(key: &str) -> Result<&str> {
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("invalid Jira project key");
    }
    Ok(key)
}

fn comment_payload(text: &str) -> Value {
    let content = text
        .split('\n')
        .map(|line| {
            let content = if line.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": line })]
            };
            json!({ "type": "paragraph", "content": content })
        })
        .collect::<Vec<_>>();
    json!({ "body": { "type": "doc", "version": 1, "content": content } })
}

fn transition_payload(id: &str) -> Value {
    json!({ "transition": { "id": id } })
}

fn render_node(node: &Value, output: &mut String, depth: usize) {
    let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" => {
            let mut text = node
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Some(marks) = node.get("marks").and_then(Value::as_array) {
                for mark in marks {
                    match mark.get("type").and_then(Value::as_str) {
                        Some("code") => text = format!("`{text}`"),
                        Some("link") => {
                            if let Some(href) = mark.pointer("/attrs/href").and_then(Value::as_str)
                            {
                                text = format!("{text} ({href})");
                            }
                        }
                        _ => {}
                    }
                }
            }
            output.push_str(&text);
        }
        "hardBreak" => output.push('\n'),
        "paragraph" => {
            render_children(node, output, depth);
            output.push('\n');
        }
        "heading" => {
            let level = node
                .pointer("/attrs/level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6);
            output.push_str(&"#".repeat(level as usize));
            output.push(' ');
            render_children(node, output, depth);
            output.push('\n');
        }
        "bulletList" | "orderedList" => {
            let mut number = node
                .pointer("/attrs/order")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if let Some(items) = node.get("content").and_then(Value::as_array) {
                for item in items {
                    output.push_str(&"  ".repeat(depth));
                    if kind == "orderedList" {
                        output.push_str(&format!("{number}. "));
                        number += 1;
                    } else {
                        output.push_str("- ");
                    }
                    render_children(item, output, depth + 1);
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
        }
        "blockquote" => {
            let mut quote = String::new();
            render_children(node, &mut quote, depth);
            for line in quote.trim_end().lines() {
                output.push_str("> ");
                output.push_str(line);
                output.push('\n');
            }
        }
        "codeBlock" => {
            output.push_str("```\n");
            render_children(node, output, depth);
            if !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str("```\n");
        }
        "inlineCard" => {
            if let Some(url) = node.pointer("/attrs/url").and_then(Value::as_str) {
                output.push_str(url);
            }
        }
        _ => render_children(node, output, depth),
    }
}

fn render_children(node: &Value, output: &mut String, depth: usize) {
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            render_node(child, output, depth);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TenantInfo {
    cloud_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiUser {
    account_id: String,
    display_name: String,
    email_address: Option<String>,
    avatar_urls: Option<std::collections::HashMap<String, String>>,
}

impl From<ApiUser> for JiraUser {
    fn from(user: ApiUser) -> Self {
        let avatar_url = user.avatar_urls.and_then(|urls| {
            urls.get("48x48")
                .cloned()
                .or_else(|| urls.into_values().next())
        });
        Self {
            account_id: user.account_id,
            display_name: user.display_name,
            email_address: user.email_address,
            avatar_url,
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    issues: Vec<ApiIssue>,
}

#[derive(Deserialize)]
struct ProjectSearchResponse {
    #[serde(default)]
    values: Vec<ApiProject>,
}

#[derive(Deserialize)]
struct ApiProject {
    key: String,
    name: String,
}

#[derive(Deserialize)]
struct ApiIssue {
    key: String,
    fields: ApiFields,
    #[serde(default)]
    names: std::collections::HashMap<String, String>,
}

#[derive(Default, Deserialize)]
struct ApiFields {
    #[serde(default)]
    summary: String,
    status: Option<NamedField>,
    priority: Option<NamedField>,
    assignee: Option<ApiDisplayUser>,
    creator: Option<ApiDisplayUser>,
    reporter: Option<ApiDisplayUser>,
    parent: Option<Box<ApiIssue>>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(rename = "issuetype")]
    issue_type: Option<NamedField>,
    #[serde(default)]
    components: Vec<NamedField>,
    #[serde(default, rename = "fixVersions")]
    fix_versions: Vec<NamedField>,
    created: Option<String>,
    updated: Option<String>,
    #[serde(rename = "duedate")]
    due_date: Option<String>,
    resolution: Option<NamedField>,
    description: Option<Value>,
    #[serde(default)]
    subtasks: Vec<ApiIssue>,
    #[serde(default)]
    issuelinks: Vec<ApiIssueLink>,
    #[serde(flatten)]
    custom: std::collections::HashMap<String, Value>,
}

#[derive(Deserialize)]
struct NamedField {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiDisplayUser {
    display_name: String,
}

#[derive(Deserialize)]
struct ApiIssueLink {
    #[serde(rename = "type")]
    link_type: ApiLinkType,
    outward_issue: Option<Box<ApiIssue>>,
    inward_issue: Option<Box<ApiIssue>>,
}

#[derive(Deserialize)]
struct ApiLinkType {
    inward: String,
    outward: String,
}

impl ApiIssue {
    fn into_summary(self) -> IssueSummary {
        IssueSummary {
            key: self.key,
            summary: self.fields.summary,
            status: field_name(self.fields.status),
            priority: self.fields.priority.map(|field| field.name),
            assignee: self.fields.assignee.map(|user| user.display_name),
        }
    }

    fn into_card(self, comments: Vec<JiraComment>) -> IssueCard {
        let sprints = self
            .names
            .iter()
            .filter(|(_, name)| name.eq_ignore_ascii_case("sprint"))
            .filter_map(|(id, _)| self.fields.custom.get(id))
            .flat_map(extract_sprint_names)
            .collect();
        let fields = self.fields;
        let links = fields
            .issuelinks
            .into_iter()
            .filter_map(|link| {
                let (relationship, issue) = if let Some(issue) = link.outward_issue {
                    (link.link_type.outward, issue)
                } else {
                    (link.link_type.inward, link.inward_issue?)
                };
                Some(IssueLink {
                    relationship,
                    key: issue.key,
                    summary: issue.fields.summary,
                    status: field_name(issue.fields.status),
                })
            })
            .collect();
        IssueCard {
            key: self.key,
            summary: fields.summary,
            status: field_name(fields.status),
            priority: fields.priority.map(|field| field.name),
            assignee: fields.assignee.map(|user| user.display_name),
            description: fields
                .description
                .as_ref()
                .map(render_adf)
                .unwrap_or_default(),
            subtasks: fields
                .subtasks
                .into_iter()
                .map(ApiIssue::into_summary)
                .collect(),
            links,
            comments,
            creator: fields.creator.map(|user| user.display_name),
            reporter: fields.reporter.map(|user| user.display_name),
            parent: fields.parent.map(|issue| issue.into_summary()),
            labels: fields.labels,
            sprints,
            issue_type: fields.issue_type.map(|field| field.name),
            components: fields
                .components
                .into_iter()
                .map(|field| field.name)
                .collect(),
            fix_versions: fields
                .fix_versions
                .into_iter()
                .map(|field| field.name)
                .collect(),
            created: fields.created,
            updated: fields.updated,
            due_date: fields.due_date,
            resolution: fields.resolution.map(|field| field.name),
            stale: false,
            stale_reason: None,
        }
    }
}

fn extract_sprint_names(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().flat_map(extract_sprint_names).collect(),
        Value::Object(object) => object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .map(|name| vec![name.to_owned()])
            .unwrap_or_default(),
        Value::String(value) => {
            let name = value
                .split_once("name=")
                .map(|(_, rest)| rest.split([',', ']']).next().unwrap_or(rest))
                .unwrap_or(value)
                .trim();
            (!name.is_empty())
                .then(|| name.to_owned())
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    }
}

fn field_name(field: Option<NamedField>) -> String {
    field.map(|field| field.name).unwrap_or_default()
}

#[derive(Deserialize)]
struct CommentsResponse {
    #[serde(default)]
    comments: Vec<ApiComment>,
}

#[derive(Deserialize)]
struct ApiComment {
    id: String,
    author: ApiDisplayUser,
    #[serde(default)]
    created: String,
    #[serde(default)]
    updated: String,
    body: Value,
}

impl ApiComment {
    fn into_comment(self) -> JiraComment {
        JiraComment {
            id: self.id,
            author: self.author.display_name,
            created: self.created,
            updated: self.updated,
            body: render_adf(&self.body),
        }
    }
}

#[derive(Deserialize)]
struct TransitionsResponse {
    #[serde(default)]
    transitions: Vec<ApiTransition>,
}

#[derive(Deserialize)]
struct ApiTransition {
    id: String,
    name: String,
    to: NamedField,
}

impl ApiTransition {
    fn into_transition(self) -> Transition {
        Transition {
            id: self.id,
            name: self.name,
            to_status: self.to.name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_common_adf_nodes() {
        let document = json!({
            "type": "doc",
            "version": 1,
            "content": [
                { "type": "heading", "attrs": { "level": 2 }, "content": [
                    { "type": "text", "text": "Details" }
                ]},
                { "type": "paragraph", "content": [
                    { "type": "text", "text": "See " },
                    { "type": "text", "text": "docs", "marks": [
                        { "type": "link", "attrs": { "href": "https://example.test" } }
                    ]}
                ]},
                { "type": "bulletList", "content": [
                    { "type": "listItem", "content": [
                        { "type": "paragraph", "content": [
                            { "type": "text", "text": "first" }
                        ]}
                    ]}
                ]},
                { "type": "blockquote", "content": [
                    { "type": "paragraph", "content": [
                        { "type": "text", "text": "quoted" }
                    ]}
                ]},
                { "type": "codeBlock", "content": [
                    { "type": "text", "text": "let x = 1;" }
                ]}
            ]
        });
        assert_eq!(
            render_adf(&document),
            "## Details\nSee docs (https://example.test)\n- first\n> quoted\n```\nlet x = 1;\n```"
        );
    }

    #[test]
    fn builds_safe_search_jql() {
        assert_eq!(
            search_jql("", None),
            "assignee = currentUser() AND resolution = Unresolved ORDER BY updated DESC"
        );
        assert_eq!(
            search_jql("OPS-1", Some("OBS")),
            "project = \"OBS\" AND (key = \"OPS-1\" OR text ~ \"OPS-1*\") ORDER BY updated DESC"
        );
        assert_eq!(
            search_jql("OPS-1", None),
            "(key = \"OPS-1\" OR text ~ \"OPS-1*\") ORDER BY updated DESC"
        );
        assert_eq!(
            search_jql("trace \" gap", None),
            "text ~ \"trace \\\" gap*\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn saves_config_without_token_and_preserves_other_sections() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("config.toml");
        fs::write(&path, "[other]\nvalue = 3\n")?;
        let config = JiraConfig {
            site: "https://example.atlassian.net".to_owned(),
            email: "user@example.test".to_owned(),
            cloud_id: "cloud-id".to_owned(),
            project: Some("OBS".to_owned()),
        };
        config.save_to(&path)?;
        let saved = fs::read_to_string(&path)?;
        assert!(saved.contains("[other]"));
        assert!(!saved.contains("token"));
        assert!(saved.contains("project = \"OBS\""));
        assert_eq!(JiraConfig::load_from(&path)?, config);
        Ok(())
    }

    #[test]
    fn multiline_comment_is_adf_paragraphs() {
        assert_eq!(
            comment_payload("one\n\nthree"),
            json!({ "body": {
                "type": "doc",
                "version": 1,
                "content": [
                    { "type": "paragraph", "content": [{ "type": "text", "text": "one" }] },
                    { "type": "paragraph", "content": [] },
                    { "type": "paragraph", "content": [{ "type": "text", "text": "three" }] }
                ]
            }})
        );
    }

    #[test]
    fn formats_jira_timestamps_in_local_time() {
        let jira_timestamp = "2026-09-01T20:36:56.613-0300";
        let expected = DateTime::parse_from_str(jira_timestamp, "%Y-%m-%dT%H:%M:%S%.f%z")
            .unwrap()
            .with_timezone(&Local)
            .format("%b %-d, %Y %H:%M")
            .to_string();
        assert_eq!(format_jira_datetime(jira_timestamp), expected);
        assert_eq!(format_jira_datetime("2026-09-01T23:36:56.613Z"), expected);
        assert_eq!(format_jira_datetime("not a timestamp"), "not a timestamp");
    }

    #[test]
    fn extracts_sprints_from_cloud_and_legacy_shapes() {
        let value = json!([
            { "id": 1, "name": "Current sprint" },
            "Future sprint",
            "com.atlassian.greenhopper.service.sprint.Sprint@123[id=2,name=Legacy sprint,state=CLOSED]",
            { "id": 3 },
            null
        ]);
        assert_eq!(
            extract_sprint_names(&value),
            vec!["Current sprint", "Future sprint", "Legacy sprint"]
        );
    }

    #[test]
    fn deserializes_transitions_and_builds_payload() -> Result<()> {
        let response: TransitionsResponse = serde_json::from_value(json!({
            "transitions": [{
                "id": "31",
                "name": "Complete",
                "to": { "name": "Done" }
            }]
        }))?;
        let transitions = response
            .transitions
            .into_iter()
            .map(ApiTransition::into_transition)
            .collect::<Vec<_>>();
        assert_eq!(
            transitions,
            vec![Transition {
                id: "31".to_owned(),
                name: "Complete".to_owned(),
                to_status: "Done".to_owned(),
            }]
        );
        assert_eq!(
            transition_payload("31"),
            json!({ "transition": { "id": "31" } })
        );
        assert!(validate_transition_id("abc").is_err());
        Ok(())
    }

    #[test]
    fn old_issue_card_cache_defaults_added_details() -> Result<()> {
        let card: IssueCard = serde_json::from_value(json!({
            "key": "OPS-1",
            "summary": "Cached issue",
            "status": "Open",
            "priority": null,
            "assignee": null,
            "description": "",
            "subtasks": [],
            "links": [],
            "comments": []
        }))?;
        assert_eq!(card.creator, None);
        assert_eq!(card.reporter, None);
        assert_eq!(card.parent, None);
        assert!(card.labels.is_empty());
        assert!(card.sprints.is_empty());
        assert_eq!(card.issue_type, None);
        assert!(card.components.is_empty());
        assert!(card.fix_versions.is_empty());
        assert_eq!(card.created, None);
        assert_eq!(card.updated, None);
        assert_eq!(card.due_date, None);
        assert_eq!(card.resolution, None);
        Ok(())
    }
}
