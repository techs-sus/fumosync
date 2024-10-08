use crate::{
	client::{Client, EditorUpdate},
	error::Error,
	login::get_session_secrets,
};
use serde::{Deserialize, Serialize};
use tracing::info;
use std::{
	ffi::OsStr,
	path::{Path, PathBuf},
};

/// fumosync.json
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
	pub script_name: String,
	pub script_id: String,
	pub whitelist: Vec<String>,
	pub is_public: bool,
}

pub async fn write_file<T: AsRef<Path>>(path: T, contents: &str) -> Result<(), Error> {
	match tokio::fs::write(path.as_ref(), contents).await {
		Ok(value) => Ok(value),
		Err(io_error) => Err(Error::CreateFile(path.as_ref().to_path_buf(), io_error)),
	}
}

async fn create_directory<T: AsRef<Path>>(path: T) -> Result<(), Error> {
	match tokio::fs::create_dir(path.as_ref()).await {
		Ok(value) => Ok(value),
		Err(io_error) => Err(Error::CreateDirectory(
			path.as_ref().to_path_buf(),
			io_error,
		)),
	}
}

pub async fn read_configuration() -> Result<Configuration, Error> {
	Ok(serde_json::from_str(&read_file("fumosync.json").await?)?)
}

/// Initializes a project for syncing within fumosclub.
pub async fn init(directory: PathBuf) -> Result<(), Error> {
	if directory.exists() {
		return Err(Error::DirectoryAlreadyExists(directory));
	}

	create_directory(directory.clone()).await?;
	create_directory(directory.join("pkg")).await?;
	create_directory(directory.join(".vscode")).await?;

	write_file(
		directory.join(".vscode").join("settings.json"),
		r#"{
	"luau-lsp.types.robloxSecurityLevel": "None",
	"luau-lsp.types.definitionFiles": ["types.d.luau"]
}"#,
	)
	.await?;

	write_file(
		directory.join("init.server.luau"),
		r#"-- you can require packages with requireM("path") where path is a file inside of pkg (no extension)"#,
	)
	.await?;

	write_file(directory.join("README.md"), r#"# stuff here"#).await?;

	write_file(
		directory.join("types.d.luau"),
		r#"declare loadstringEnabled: boolean
declare owner: Player
declare arguments: { any }

declare isolatedStorage: {
  get: (name: string) -> any,
  set: (name: string, value: any?) -> ()
}

declare immediateSignals: boolean
declare NLS: (source: string, parent: Instance?) -> LocalScript
declare requireM: (moduleName: string) -> any

declare LoadAssets: (assetId: number) -> {
  Get: (asset: string) -> Instance,
  Exists: (asset: string) -> boolean,
  GetNames: () -> { string },
  GetArray: () -> { Instance },
  GetDictionary: () -> { [string]: Instance }
}"#,
	)
	.await?;

	write_file(
		directory.join("fumosync.json"),
		&serde_json::to_string_pretty(&Configuration {
			script_name: directory
				.file_name()
				.unwrap_or(OsStr::new("unknown"))
				.to_string_lossy()
				.to_string(),
			script_id: "???".to_owned(),
			whitelist: Vec::new(),
			is_public: false,
		})?,
	)
	.await?;

	Ok(())
}

/// Pulls a project from fumosclub and links it via fumosync.json.
pub async fn pull_project(script_id: String, project_directory: PathBuf) -> Result<(), Error> {
	let client = Client::new(get_session_secrets().await?);

	// setup initial file structure for hydration
	match init(project_directory.clone()).await {
		Ok(_) => {}
		Err(e) => return Err(Error::ProjectDidntInitialize(Box::new(e))),
	};

	let script_info = client.get_editor(&script_id).await?.script_info;

	write_file(
		project_directory.join("README.md"),
		&script_info.description,
	)
	.await?;

	write_file(
		project_directory.join("init.server.luau"),
		&script_info.source.main,
	)
	.await?;

	write_file(
		project_directory.join("fumosync.json"),
		&serde_json::to_string_pretty(&Configuration {
			script_name: script_info.name,
			script_id,
			whitelist: script_info.whitelist,
			is_public: script_info.is_public,
		})?,
	)
	.await?;

	for (name, source) in script_info.source.modules {
		write_file(
			project_directory.join("pkg").join(format!("{name}.luau")),
			&source,
		)
		.await?;
	}

	Ok(())
}

pub async fn read_file<T: AsRef<Path>>(path: T) -> Result<String, Error> {
	match tokio::fs::read_to_string(path.as_ref()).await {
		Ok(value) => Ok(value),
		Err(io_error) => Err(Error::ReadFile(path.as_ref().to_path_buf(), io_error)),
	}
}

pub async fn push_project() -> Result<(), Error> {
	let configuration = read_configuration().await?;
	let whitelist = configuration.whitelist.iter().map(|x| x.as_str()).collect();

	let description = &read_file("README.md").await?;
	let main_source = &read_file("init.server.luau").await?;

	let mut actions: Vec<EditorUpdate> = Vec::from([
		EditorUpdate::Name(&configuration.script_name),
		EditorUpdate::Whitelist(whitelist),
		EditorUpdate::Publicity(configuration.is_public),
		EditorUpdate::Description(description),
		EditorUpdate::MainSource(main_source),
	]);

	let mut modules: Vec<(String, String)> = Vec::new();

	let pkg_path = PathBuf::from("pkg");
	let mut stream = match tokio::fs::read_dir(&pkg_path).await {
		Ok(value) => value,
		Err(io_error) => return Err(Error::ReadDirectory(pkg_path, io_error)),
	};

	while let Some(module) = stream.next_entry().await? {
		if let Ok(file_type) = module.file_type().await {
			if file_type.is_file()
				&& module
					.path()
					.extension()
					.unwrap_or(OsStr::new(""))
					.to_string_lossy()
					== "luau"
			{
				let path_without_extension = PathBuf::from(module.file_name()).with_extension("");
				let name = path_without_extension.to_string_lossy();
				let source: String = read_file(module.path()).await?;
				modules.push((name.to_string(), source));
			}
		} else {
			info!("failed getting file type for {}", module.path().display());
		}
	}

	// use .iter() to force items to have a lifetime bounded by the function
	for (name, source) in modules.iter() {
		actions.push(EditorUpdate::Module { name, source });
	}

	let client = Client::new(get_session_secrets().await?);
	client
		.set_editor(&configuration.script_id, &actions)
		.await?;
	Ok(())
}
