use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use console::Style;
use minijinja::Environment;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use similar::{ChangeTag, TextDiff};
use dialoguer::Confirm;
use std::fmt;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory containing service templates
    #[arg(long, default_value = "templates")]
    templates: PathBuf,

    /// Force apply changes even if state is outdated
    #[arg(long)]
    force: bool,

    /// File containing the configuration for the template.
    #[arg(short, long)]
    input: String,

    /// File that will store the state file
    #[arg(short, long)]
    state: String,
}

#[derive(Debug)]
enum ManagerError {
    Io(std::io::Error),
    Template(minijinja::Error),
    Yaml(serde_yaml::Error),
    TemplateNotFound(PathBuf),
    StateOutOfSync(String),
}

impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManagerError::Io(err) => write!(f, "IO error: {}", err),
            ManagerError::Template(err) => write!(f, "Template error: {}", err),
            ManagerError::Yaml(err) => write!(f, "YAML error: {}", err),
            ManagerError::TemplateNotFound(path) => write!(f, "Template not found: {}", path.display()),
            ManagerError::StateOutOfSync(service) => write!(f, "Service {} has been modified outside of this tool", service),
        }
    }
}

impl std::error::Error for ManagerError {}

impl From<std::io::Error> for ManagerError {
    fn from(err: std::io::Error) -> ManagerError {
        ManagerError::Io(err)
    }
}

impl From<minijinja::Error> for ManagerError {
    fn from(err: minijinja::Error) -> ManagerError {
        ManagerError::Template(err)
    }
}

impl From<serde_yaml::Error> for ManagerError {
    fn from(err: serde_yaml::Error) -> ManagerError {
        ManagerError::Yaml(err)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ServiceConfig {
    template: String,
    unit: String,
    variables: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    services: Vec<ServiceConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    services: HashMap<String, String>,
}

#[derive(Debug)]
struct ServiceChange {
    unit: String,
    old_content: Option<String>,
    new_content: String,
    state_modified: bool,
}

impl StateFile {
    fn load_or_create(path: &Path) -> Result<Self, ManagerError> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            Ok(serde_yaml::from_str(&content).unwrap_or_else(|_| StateFile {
                services: HashMap::new(),
            }))
        } else {
            Ok(StateFile {
                services: HashMap::new(),
            })
        }
    }

    fn save(&self, path: &Path) -> Result<(), ManagerError> {
        let content = serde_yaml::to_string(self)?;
        Ok(fs::write(path, content)?)
    }

    fn validate_service(&self, unit: &str, content: &str) -> bool {
        match self.services.get(unit) {
            Some(stored_hash) => calculate_hash(content) == *stored_hash,
            None => true,
        }
    }
}

fn calculate_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn render_template(template_dir: &Path, template_name: &str, variables: &HashMap<String, String>) -> Result<String, ManagerError> {
    let template_path = template_dir.join(template_name);
    if !template_path.exists() {
        return Err(ManagerError::TemplateNotFound(template_path));
    }
    
    let template_content = fs::read_to_string(&template_path)?;
    let mut env = Environment::new();
    env.add_template("service", &template_content)?;
    
    let template = env.get_template("service")?;
    Ok(template.render(variables)?)
}

fn print_diff(old_content: Option<&str>, new_content: &str, unit: &str, state_modified: bool) {
    let old_content = old_content.unwrap_or("");
    let diff = TextDiff::from_lines(old_content, new_content);
    
    println!("\nChanges for {}:", unit);
    if state_modified {
        println!("⚠️  WARNING: This service has been modified outside of this tool!");
    }
    println!("----------------------------");
    
    for change in diff.iter_all_changes() {
        let (sign, style) = match change.tag() {
            ChangeTag::Delete => ("-", Style::new().red()),
            ChangeTag::Insert => ("+", Style::new().green()),
            ChangeTag::Equal => (" ", Style::new()),
        };
        
        print!("{}", style.apply_to(format!("{}{}", sign, change)));
    }
    println!("----------------------------\n");
}

fn preview_changes(
    config: &ServiceConfig,
    template_dir: &Path,
    state: &StateFile,
) -> Result<ServiceChange, ManagerError> {
    let new_content = render_template(template_dir, &config.template, &config.variables)?;
    let service_path = Path::new("/etc/systemd/system").join(&config.unit);
    
    let (old_content, state_modified) = if service_path.exists() {
        let content = fs::read_to_string(&service_path)?;
        let valid = state.validate_service(&config.unit, &content);
        (Some(content), !valid)
    } else {
        (None, false)
    };
    
    Ok(ServiceChange {
        unit: config.unit.clone(),
        old_content,
        new_content,
        state_modified,
    })
}

fn sync_service(change: &ServiceChange, state: &mut StateFile) -> Result<(), ManagerError> {
    let service_path = Path::new("/etc/systemd/system").join(&change.unit);
    let new_hash = calculate_hash(&change.new_content);
    
    fs::write(&service_path, &change.new_content)?;
    
    // need to reload the daemon so it picks up the updated service
    std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .status()?;
        
    std::process::Command::new("systemctl")
        .args(["restart", &change.unit])
        .status()?;
        
    state.services.insert(change.unit.clone(), new_hash);
    
    Ok(())
}

fn main() -> Result<(), ManagerError> {
    let args = Args::parse();
    
    let config_content = fs::read_to_string(&args.input)?;
    let config: Config = serde_yaml::from_str(&config_content)?;
    
    let state_path = Path::new(&args.state);
    let mut state = StateFile::load_or_create(state_path)?;
    
    let mut changes: Vec<ServiceChange> = Vec::new();
    
    println!("Analyzing changes...");
    for service_config in &config.services {
        let change = preview_changes(service_config, &args.templates, &state)?;
        
        let needs_update = match &change.old_content {
            Some(old_content) => old_content != &change.new_content,
            None => true,
        };
        
        if needs_update {
            // if state is modified and --force is not used, return error
            if change.state_modified && !args.force {
                return Err(ManagerError::StateOutOfSync(change.unit.clone()));
            }
            changes.push(change);
        }
    }
    
    if changes.is_empty() {
        println!("No changes needed for any services");
        return Ok(());
    }
    
    println!("\nPlanned changes:");
    for change in &changes {
        print_diff(
            change.old_content.as_deref(),
            &change.new_content,
            &change.unit,
            change.state_modified,
        );
    }
    
    println!("The following actions will be performed:");
    for change in &changes {
        if change.state_modified {
            println!(" ! Override manual changes to: {}", change.unit);
        }
        println!(" * Update service unit file: {}", change.unit);
        println!(" * Reload systemd daemon");
        println!(" * Restart service: {}", change.unit);
    }
    
    if !Confirm::new()
        .with_prompt("Do you want to apply these changes?")
        .interact()? 
    {
        println!("Operation cancelled.");
        return Ok(());
    }
    
    println!("Applying changes...");
    for change in &changes {
        println!("Updating service: {}", change.unit);
        sync_service(change, &mut state)?;
    }
    
    state.save(state_path)?;
    
    println!("All changes applied successfully!");
    
    Ok(())
}
