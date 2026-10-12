//! Doctor's incomplete-output guidance stays bound to the selected wiki and settings.
use super::{
    Arguments,
    remote::{RemoteArguments, RemoteContext},
};
use crate::{config::local::LoadedUserConfig, domain::*, vault::discovery::SelectedWiki};

pub(super) fn decorate(
    error: &mut WikiError,
    args: &Arguments,
    local: &LoadedUserConfig,
    selected: &SelectedWiki,
    remote: &RemoteArguments,
    context: &RemoteContext<'_>,
    extraction_schema: bool,
) {
    if error.details["cause"]["reason"] != "incomplete_max_output_tokens" {
        return;
    }
    // A command that cannot be quoted safely leaves the original allowance facts
    // visible; it must not replace a paid failure or guess another write target.
    if let Some(guidance) = guidance(
        error,
        args,
        local,
        selected,
        remote,
        context,
        extraction_schema,
    ) {
        error.hint = Some(guidance.hint.clone());
        error.details["next_action"] = guidance.hint.into();
        error.details["accounting_command"] = guidance.accounting.into();
        if let Some(repair) = guidance.repair {
            error.details["repair_command"] = repair.into();
        }
    }
}

struct Guidance {
    hint: String,
    accounting: String,
    repair: Option<String>,
}

fn guidance(
    error: &WikiError,
    args: &Arguments,
    local: &LoadedUserConfig,
    selected: &SelectedWiki,
    remote: &RemoteArguments,
    context: &RemoteContext<'_>,
    extraction_schema: bool,
) -> Option<Guidance> {
    let cause = &error.details["cause"];
    let allowance = cause["max_output_tokens"].as_u64()?;
    let service_cap = cause["effective_service_cap"].as_u64()?;
    let units_ceiling = cause["output_units_ceiling"].as_u64();
    let run = error.details["run_id"].as_str()?;
    let mut prefix = vec![
        "lwiki".to_owned(),
        "--wiki".into(),
        selected.root.path().to_str()?.into(),
        "--expect-wiki-id".into(),
        selected.wiki_id.to_string(),
    ];
    if let Some(path) = local.path() {
        prefix.extend(["--preferences".into(), path.to_str()?.into()]);
    }
    if args.no_user_config {
        prefix.push("--no-user-config".into());
    }
    let mut accounting = prefix.clone();
    accounting.extend(["jobs".into(), "status".into(), "--run".into(), run.into()]);
    let accounting = quote(&accounting)?;
    let mut hint = format!(
        "Reasoning tokens can use this allowance before the model returns JSON. Inspect retained accounting with: {accounting}. "
    );
    let repair = if allowance < service_cap {
        hint.push_str(&format!(
            "Repeat your original doctor command with --max-output-units TOKENS. Choose TOKENS above {allowance} and at most {service_cap}. Keep the same wiki, preferences, provider file, profile and all other request, cost, byte and deadline limits."
        ));
        None
    } else {
        let effective = remote.effective(context);
        let provider_path = match effective.providers_config {
            Some(path) => path,
            None if context.allow_ambient_provider => {
                crate::app::remote::default_provider_path().ok()?
            }
            None => return None,
        };
        let profile = context.profile?;
        if local.path().is_none() {
            prefix.extend(["--preferences".into(), "FILE".into()]);
        }
        prefix.extend([
            "--profile".into(),
            profile.into(),
            "config".into(),
            "set".into(),
            "generation".into(),
            "--providers-config".into(),
            provider_path.to_str()?.into(),
            "--max-output-tokens".into(),
            "TOKENS".into(),
            "--grant-provider".into(),
            "--non-interactive".into(),
        ]);
        let command = quote(&prefix)?;
        hint.push_str(&format!(
            "Increase the model output limit with: {command}. Replace TOKENS with a larger limit supported by your model. "
        ));
        if local.path().is_none() {
            hint.push_str("Replace FILE with an absolute path for your preferences file and use that same file when rerunning. ");
        } else {
            let saved_path = local.toml_migration_target();
            let path = saved_path.as_deref().or(local.path())?;
            let preference_flag = quote(&["--preferences".into(), path.to_str()?.into()])?;
            hint.push_str(&format!("Use {preference_flag} when rerunning. "));
        }
        hint.push_str("After a successful edit, repeat your original doctor command for this wiki and provider file. Omit its old --profile option so the newly saved model selection is used. Keep all other request, cost, byte and deadline limits.");
        if units_ceiling.is_some_and(|ceiling| ceiling <= allowance) {
            hint.push_str(
                " Also raise --max-output-units above the reached allowance when rerunning.",
            );
        }
        Some(command)
    };
    if extraction_schema {
        hint.push_str(" Keep --extraction-schema when rerunning.");
    }
    hint.push_str(" Each rerun creates a new probe run and preserves prior charges.");
    Some(Guidance {
        hint,
        accounting,
        repair,
    })
}

fn quote(words: &[String]) -> Option<String> {
    shlex::try_join(words.iter().map(String::as_str)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::{
            Command,
            arguments::{ConfigCommand, JobsCommand},
        },
        config::{self, DefaultWiki},
    };
    use clap::Parser;
    use std::{fs, path::PathBuf};

    struct Fixture {
        _temp: tempfile::TempDir,
        loaded: LoadedUserConfig,
        selected: SelectedWiki,
        args: Arguments,
        providers: PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let a = base.join("Default A");
        let b = base.join("B owner's notes");
        for (path, id) in [(&a, "Vault.A"), (&b, "Vault.B")] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("WIKI.md"), format!("---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: vault\ntitle: Doctor fixture\n---\n")).unwrap();
        }
        let preferences = base.join("Owner's preferences.toml");
        let providers = base.join("B owner's providers.toml");
        let mut document = config::LocalConfigDocument {
            default_wiki: Some(DefaultWiki {
                path: a,
                wiki_id: RecordId::new("Vault.A").unwrap(),
            }),
            providers_config: Some(base.join("A providers.toml")),
            ..Default::default()
        };
        document.defaults.profile = Some("saved_elsewhere".into());
        fs::write(&preferences, toml::to_string(&document).unwrap()).unwrap();
        let loaded = config::local::load_user_config(Some(&preferences), false).unwrap();
        let selected =
            crate::vault::discovery::select(Some(&b), &base, loaded.default_wiki()).unwrap();
        let args = Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            b.to_str().unwrap(),
            "--expect-wiki-id",
            "Vault.B",
            "--preferences",
            preferences.to_str().unwrap(),
            "--profile",
            "manual",
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--providers-config",
            providers.to_str().unwrap(),
            "--max-requests",
            "1",
            "--max-cost",
            "0.05",
            "--deadline-ms",
            "20000",
        ])
        .unwrap();
        Fixture {
            _temp: temp,
            loaded,
            selected,
            args,
            providers,
        }
    }

    fn remote(args: &Arguments) -> &RemoteArguments {
        let Command::Doctor { remote, .. } = &args.command else {
            panic!("not a doctor command");
        };
        remote
    }

    fn failure(allowance: u64, service_cap: u64, units: Option<u64>) -> WikiError {
        let mut error = WikiError::new(
            ErrorCode::ProviderResponse,
            "probe reached output allowance",
        );
        error.details = serde_json::json!({"run_id":"Run.Paid", "cause":{
            "reason":"incomplete_max_output_tokens", "max_output_tokens":allowance,
            "effective_service_cap":service_cap, "output_units_ceiling":units
        }});
        error
    }

    fn decorate_fixture(f: &Fixture, error: &mut WikiError) {
        let context = RemoteContext {
            profile: Some("manual"),
            wiki_id: None,
            document: f.loaded.document(),
            allow_ambient_provider: true,
        };
        decorate(
            error,
            &f.args,
            &f.loaded,
            &f.selected,
            remote(&f.args),
            &context,
            true,
        );
    }

    fn parse_recipe(command: &str) -> Arguments {
        let words = shlex::split(command).unwrap().into_iter().map(|word| {
            if word == "TOKENS" {
                "16384".into()
            } else {
                word
            }
        });
        Arguments::try_parse_from(words).unwrap()
    }

    #[test]
    fn repair_and_accounting_pin_b_instead_of_default_a_with_quoted_overrides() {
        let f = fixture();
        let mut error = failure(8192, 8192, None);
        decorate_fixture(&f, &mut error);
        let repaired = parse_recipe(error.details["repair_command"].as_str().unwrap());
        assert_eq!(repaired.wiki.as_deref(), Some(f.selected.root.path()));
        assert_ne!(
            repaired.wiki.as_deref(),
            f.loaded.default_wiki().map(|wiki| wiki.path.as_path())
        );
        assert_eq!(repaired.preferences.as_deref(), f.loaded.path());
        assert_eq!(repaired.expect_wiki_id.as_ref(), Some(&f.selected.wiki_id));
        assert_eq!(repaired.profile.as_deref(), Some("manual"));
        let Command::Config {
            command: ConfigCommand::Set(settings),
        } = repaired.command
        else {
            panic!("not config set");
        };
        assert_eq!(settings.providers_config, Some(f.providers.clone()));
        assert_eq!(settings.max_output_tokens, Some(16384));
        assert!(settings.grant_provider && settings.non_interactive);
        let accounting = parse_recipe(error.details["accounting_command"].as_str().unwrap());
        assert_eq!(accounting.wiki.as_deref(), Some(f.selected.root.path()));
        assert_eq!(accounting.preferences.as_deref(), f.loaded.path());
        assert_eq!(
            accounting.expect_wiki_id.as_ref(),
            Some(&f.selected.wiki_id)
        );
        assert!(
            matches!(accounting.command, Command::Jobs { command: JobsCommand::Status { run } } if run.as_str() == "Run.Paid")
        );
        let hint = error.hint.unwrap();
        assert!(hint.contains("Omit its old --profile"));
        assert!(hint.contains("Keep all other request, cost, byte and deadline limits"));
        assert!(hint.contains("Keep --extraction-schema"));
    }

    #[test]
    fn binding_units_below_model_cap_need_no_config_edit_or_grant() {
        let f = fixture();
        let mut error = failure(64, 8192, Some(64));
        decorate_fixture(&f, &mut error);
        assert!(error.details.get("repair_command").is_none());
        let hint = error.hint.unwrap();
        assert!(hint.contains("--max-output-units TOKENS"));
        assert!(hint.contains("above 64 and at most 8192"));
        assert!(hint.contains("provider file, profile and all other"));
        assert!(!hint.contains("config set"));
        assert!(!hint.contains("--grant-provider"));
        assert!(!hint.contains("Omit its old --profile"));
        let mut tied = failure(8192, 8192, Some(8192));
        decorate_fixture(&f, &mut tied);
        assert!(tied.details["repair_command"].is_string());
        assert!(tied.hint.unwrap().contains("Also raise --max-output-units"));
    }

    #[test]
    fn disabled_preferences_require_an_explicit_repair_destination_and_matching_retry() {
        let f = fixture();
        let loaded = config::local::load_user_config(None, true).unwrap();
        let args = Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            f.selected.root.path().to_str().unwrap(),
            "--no-user-config",
            "--profile",
            "manual",
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--providers-config",
            f.providers.to_str().unwrap(),
        ])
        .unwrap();
        let context = RemoteContext {
            profile: Some("manual"),
            wiki_id: None,
            document: loaded.document(),
            allow_ambient_provider: false,
        };
        let mut error = failure(4096, 4096, None);
        decorate(
            &mut error,
            &args,
            &loaded,
            &f.selected,
            remote(&args),
            &context,
            false,
        );
        let repaired = parse_recipe(error.details["repair_command"].as_str().unwrap());
        assert!(repaired.no_user_config);
        assert_eq!(repaired.preferences, Some(PathBuf::from("FILE")));
        assert_eq!(repaired.wiki.as_deref(), Some(f.selected.root.path()));
        let accounting = parse_recipe(error.details["accounting_command"].as_str().unwrap());
        assert!(accounting.no_user_config);
        assert!(accounting.preferences.is_none());
        let hint = error.hint.unwrap();
        assert!(hint.contains("Replace FILE with an absolute path"));
        assert!(hint.contains("use that same file when rerunning"));
    }

    #[test]
    fn unrelated_failures_are_unchanged() {
        let f = fixture();
        let mut error = failure(4096, 4096, None);
        error.details["cause"]["reason"] = "schema_invalid".into();
        error.hint = Some("original hint".into());
        let original = error.clone();
        decorate_fixture(&f, &mut error);
        assert_eq!(error, original);
    }
}
