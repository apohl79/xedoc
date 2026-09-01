mod requirements_exec_policy_tests {
    use crate::exec_policy::load_exec_policy;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use tempfile::tempdir;
    use toml::Value as TomlValue;
    use toml::from_str;
    use xedoc_config::ConfigLayerEntry;
    use xedoc_config::ConfigLayerSource;
    use xedoc_config::ConfigLayerStack;
    use xedoc_config::ConfigRequirements;
    use xedoc_config::ConfigRequirementsToml;
    use xedoc_config::ConfigRequirementsWithSources;
    use xedoc_config::RequirementSource;
    use xedoc_config::RequirementsExecPolicyDecisionToml;
    use xedoc_config::RequirementsExecPolicyParseError;
    use xedoc_config::RequirementsExecPolicyPatternTokenToml;
    use xedoc_config::RequirementsExecPolicyPrefixRuleToml;
    use xedoc_config::RequirementsExecPolicyToml;
    use xedoc_execpolicy::Decision;
    use xedoc_execpolicy::Evaluation;
    use xedoc_execpolicy::RuleMatch;
    use xedoc_utils_absolute_path::AbsolutePathBuf;

    fn tokens(cmd: &[&str]) -> Vec<String> {
        cmd.iter().map(std::string::ToString::to_string).collect()
    }

    fn panic_if_called(_: &[String]) -> Decision {
        panic!("rule should match so heuristic should not be called");
    }

    fn config_stack_for_dot_xedoc_folder_with_requirements(
        dot_xedoc_folder: &Path,
        requirements: ConfigRequirements,
    ) -> ConfigLayerStack {
        let dot_xedoc_folder = AbsolutePathBuf::from_absolute_path(dot_xedoc_folder)
            .expect("absolute dot_xedoc_folder");
        let layer = ConfigLayerEntry::new(
            ConfigLayerSource::Project { dot_xedoc_folder },
            TomlValue::Table(Default::default()),
        );
        ConfigLayerStack::new(vec![layer], requirements, ConfigRequirementsToml::default())
            .expect("ConfigLayerStack")
    }

    fn requirements_from_toml(toml_str: &str) -> ConfigRequirements {
        let config: ConfigRequirementsToml = from_str(toml_str).expect("parse requirements toml");
        let mut with_sources = ConfigRequirementsWithSources::default();
        with_sources.merge_unset_fields(RequirementSource::Unknown, config);
        ConfigRequirements::try_from(with_sources).expect("requirements")
    }

    #[test]
    fn parses_single_prefix_rule_from_raw_toml() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;

        assert_eq!(
            parsed,
            RequirementsExecPolicyToml {
                prefix_rules: vec![RequirementsExecPolicyPrefixRuleToml {
                    pattern: vec![RequirementsExecPolicyPatternTokenToml {
                        token: Some("rm".to_string()),
                        any_of: None,
                    }],
                    decision: Some(RequirementsExecPolicyDecisionToml::Forbidden),
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn parses_multiple_prefix_rules_from_raw_toml() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
    { pattern = [{ token = "git" }, { any_of = ["push", "commit"] }], decision = "prompt", justification = "review changes before push or commit" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;

        assert_eq!(
            parsed,
            RequirementsExecPolicyToml {
                prefix_rules: vec![
                    RequirementsExecPolicyPrefixRuleToml {
                        pattern: vec![RequirementsExecPolicyPatternTokenToml {
                            token: Some("rm".to_string()),
                            any_of: None,
                        }],
                        decision: Some(RequirementsExecPolicyDecisionToml::Forbidden),
                        justification: None,
                    },
                    RequirementsExecPolicyPrefixRuleToml {
                        pattern: vec![
                            RequirementsExecPolicyPatternTokenToml {
                                token: Some("git".to_string()),
                                any_of: None,
                            },
                            RequirementsExecPolicyPatternTokenToml {
                                token: None,
                                any_of: Some(vec!["push".to_string(), "commit".to_string()]),
                            },
                        ],
                        decision: Some(RequirementsExecPolicyDecisionToml::Prompt),
                        justification: Some("review changes before push or commit".to_string()),
                    },
                ],
            }
        );

        Ok(())
    }

    #[test]
    fn converts_rules_toml_into_internal_policy_representation() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "forbidden" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let policy = parsed.to_policy()?;

        assert_eq!(
            policy.check(&tokens(&["rm", "-rf", "/tmp"]), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["rm"]),
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn head_any_of_expands_into_multiple_program_rules() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ any_of = ["git", "hg"] }, { token = "status" }], decision = "prompt" },
]
"#;
        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let policy = parsed.to_policy()?;

        assert_eq!(
            policy.check(&tokens(&["git", "status"]), &panic_if_called),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["git", "status"]),
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );
        assert_eq!(
            policy.check(&tokens(&["hg", "status"]), &panic_if_called),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: tokens(&["hg", "status"]),
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[test]
    fn missing_decision_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }] },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("missing decision");

        assert!(matches!(
            err,
            RequirementsExecPolicyParseError::MissingDecision { rule_index: 0 }
        ));
        Ok(())
    }

    #[test]
    fn allow_decision_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = [
    { pattern = [{ token = "rm" }], decision = "allow" },
]
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("allow decision not allowed");

        assert!(matches!(
            err,
            RequirementsExecPolicyParseError::AllowDecisionNotAllowed { rule_index: 0 }
        ));
        Ok(())
    }

    #[test]
    fn empty_prefix_rules_is_rejected() -> anyhow::Result<()> {
        let toml_str = r#"
prefix_rules = []
"#;

        let parsed: RequirementsExecPolicyToml = from_str(toml_str)?;
        let err = parsed.to_policy().expect_err("empty prefix rules");

        assert!(matches!(
            err,
            RequirementsExecPolicyParseError::EmptyPrefixRules
        ));
        Ok(())
    }

    #[tokio::test]
    async fn loads_requirements_exec_policy_without_rules_files() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let requirements = requirements_from_toml(
            r#"
                [rules]
                prefix_rules = [
                    { pattern = [{ token = "rm" }], decision = "forbidden" },
                ]
            "#,
        );
        let config_stack =
            config_stack_for_dot_xedoc_folder_with_requirements(temp_dir.path(), requirements);

        let policy = load_exec_policy(&config_stack).await?;

        assert_eq!(
            policy.check_multiple([vec!["rm".to_string()]].iter(), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["rm".to_string()],
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }

    #[tokio::test]
    async fn merges_requirements_exec_policy_with_file_rules() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let policy_dir = temp_dir.path().join("rules");
        std::fs::create_dir_all(&policy_dir)?;
        std::fs::write(
            policy_dir.join("deny.rules"),
            r#"prefix_rule(pattern=["rm"], decision="forbidden")"#,
        )?;

        let requirements = requirements_from_toml(
            r#"
                [rules]
                prefix_rules = [
                    { pattern = [{ token = "git" }, { token = "push" }], decision = "prompt" },
                ]
            "#,
        );
        let config_stack =
            config_stack_for_dot_xedoc_folder_with_requirements(temp_dir.path(), requirements);

        let policy = load_exec_policy(&config_stack).await?;

        assert_eq!(
            policy.check_multiple([vec!["rm".to_string()]].iter(), &panic_if_called),
            Evaluation {
                decision: Decision::Forbidden,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["rm".to_string()],
                    decision: Decision::Forbidden,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );
        assert_eq!(
            policy.check_multiple(
                [vec!["git".to_string(), "push".to_string()]].iter(),
                &panic_if_called
            ),
            Evaluation {
                decision: Decision::Prompt,
                matched_rules: vec![RuleMatch::PrefixRuleMatch {
                    matched_prefix: vec!["git".to_string(), "push".to_string()],
                    decision: Decision::Prompt,
                    resolved_program: None,
                    justification: None,
                }],
            }
        );

        Ok(())
    }
}
