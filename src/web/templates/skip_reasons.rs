use super::{SkipReasonGroupView, SkipReasonView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipReasonGroupKind {
    Matcher,
    Linking,
    Cleanup,
    AutoAcquire,
}

impl SkipReasonGroupKind {
    fn label(self) -> &'static str {
        match self {
            Self::Matcher => "Matcher",
            Self::Linking => "Linking",
            Self::Cleanup => "Cleanup",
            Self::AutoAcquire => "Auto-Acquire",
        }
    }
}

#[derive(Debug, Clone)]
struct SkipReasonPresentation {
    group: SkipReasonGroupKind,
    label: String,
    help: String,
}

fn title_case_token(token: &str) -> String {
    match token {
        "" => String::new(),
        "api" => "API".to_string(),
        "dmm" => "DMM".to_string(),
        "id" => "ID".to_string(),
        "rd" => "RD".to_string(),
        "tv" => "TV".to_string(),
        "ui" => "UI".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => {
                    let mut title = first.to_uppercase().collect::<String>();
                    title.push_str(chars.as_str());
                    title
                }
                None => String::new(),
            }
        }
    }
}

fn humanize_skip_reason(reason: &str) -> String {
    let tail = reason
        .strip_prefix("matcher_")
        .or_else(|| reason.strip_prefix("auto_acquire_"))
        .unwrap_or(reason);
    tail.split('_')
        .filter(|token| !token.is_empty())
        .map(title_case_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn skip_reason_group_kind(reason: &str) -> SkipReasonGroupKind {
    match reason {
        "ambiguous_match" => SkipReasonGroupKind::Matcher,
        "directory_guard" | "not_symlink" | "source_or_target_invalid" => {
            SkipReasonGroupKind::Cleanup
        }
        _ if reason.starts_with("matcher_") => SkipReasonGroupKind::Matcher,
        _ if reason.starts_with("auto_acquire_") => SkipReasonGroupKind::AutoAcquire,
        _ => SkipReasonGroupKind::Linking,
    }
}

fn skip_reason_presentation(reason: &str) -> SkipReasonPresentation {
    match reason {
        "ambiguous_match" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Ambiguous match".to_string(),
            help: "Multiple candidates scored too closely, so Symlinkarr refused to guess."
                .to_string(),
        },
        "matcher_no_library_candidates" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "No library candidates".to_string(),
            help: "The parser did not produce any plausible library candidates for this source."
                .to_string(),
        },
        "matcher_exact_id_incompatible" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Exact-ID candidate incompatible".to_string(),
            help: "A media ID was found in the source, but shape or metadata checks rejected it."
                .to_string(),
        },
        "matcher_episode_mapping_unresolved" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Episode mapping unresolved".to_string(),
            help: "The matcher could not resolve season or episode context for the candidate."
                .to_string(),
        },
        "matcher_media_shape_mismatch" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Media shape mismatch".to_string(),
            help: "The source looked like the wrong shape for the candidate, such as movie vs episode."
                .to_string(),
        },
        "matcher_metadata_mismatch" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Metadata mismatch".to_string(),
            help: "Year, season, or other metadata disagreed with the candidate item."
                .to_string(),
        },
        "matcher_empty_parsed_title" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Parsed title empty".to_string(),
            help: "The parser did not leave enough title text to score aliases safely."
                .to_string(),
        },
        "matcher_missing_aliases" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "No aliases available".to_string(),
            help: "The library item had no usable aliases to compare against the source title."
                .to_string(),
        },
        "matcher_alias_score_below_threshold" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "Alias score below threshold".to_string(),
            help: "Candidates existed, but name similarity stayed below the configured threshold."
                .to_string(),
        },
        "matcher_no_candidate" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Matcher,
            label: "No surviving candidate".to_string(),
            help: "Candidates were considered, but none survived the full matcher pipeline."
                .to_string(),
        },
        "already_correct" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Already correct".to_string(),
            help: "The target already pointed at the intended source, so no update was needed."
                .to_string(),
        },
        "already_correct_disk" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Already correct on disk".to_string(),
            help: "The filesystem already had the desired symlink, so Symlinkarr backfilled state."
                .to_string(),
        },
        "source_missing_before_link" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Source missing before link".to_string(),
            help: "The source disappeared before Symlinkarr could create or update the symlink."
                .to_string(),
        },
        "source_unreadable_before_link" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Source unreadable before link".to_string(),
            help: "The source still existed, but could not be read safely at link time."
                .to_string(),
        },
        "regular_file_guard" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Regular-file guard".to_string(),
            help: "The target path already contained a normal file, so Symlinkarr refused to overwrite it."
                .to_string(),
        },
        "dry_run" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Linking,
            label: "Dry-run preview".to_string(),
            help: "The linker intentionally stopped before writing because this run was a dry run."
                .to_string(),
        },
        "directory_guard" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Cleanup,
            label: "Directory guard".to_string(),
            help: "Dead-link cleanup skipped this path because it resolved to a directory."
                .to_string(),
        },
        "not_symlink" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Cleanup,
            label: "Not a symlink".to_string(),
            help: "Cleanup found a normal path where a tracked symlink was expected."
                .to_string(),
        },
        "source_or_target_invalid" => SkipReasonPresentation {
            group: SkipReasonGroupKind::Cleanup,
            label: "Source or target invalid".to_string(),
            help: "The tracked link pointed at a broken or otherwise invalid source or target."
                .to_string(),
        },
        "auto_acquire_queue_capacity_deferred" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Queue capacity deferred".to_string(),
            help: "The acquisition queue hit its current limit, so Symlinkarr paused new work."
                .to_string(),
        },
        "auto_acquire_download_failed" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Download failed".to_string(),
            help: "A submitted acquisition did not finish cleanly.".to_string(),
        },
        "auto_acquire_completed_linked" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Completed and linked".to_string(),
            help: "The acquisition completed and Symlinkarr observed the relink.".to_string(),
        },
        "auto_acquire_relink_timeout" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Relink timeout".to_string(),
            help: "The download completed, but Symlinkarr did not observe the relink before timeout."
                .to_string(),
        },
        "auto_acquire_queue_failing" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Queue failing guard".to_string(),
            help: "The queue entered a failing state and blocked new submissions.".to_string(),
        },
        "auto_acquire_provider_pending" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Provider still pending".to_string(),
            help: "The provider reported the request as pending rather than ready to submit."
                .to_string(),
        },
        "auto_acquire_no_result_provider_fallback_exhausted" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "No result from providers".to_string(),
            help: "Prowlarr returned nothing usable and the DMM fallback also found no usable result."
                .to_string(),
        },
        "auto_acquire_no_result_prowlarr_empty" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "No Prowlarr result".to_string(),
            help: "Prowlarr returned no usable release for the query variants tried."
                .to_string(),
        },
        "auto_acquire_no_result_dmm_empty" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "No DMM result".to_string(),
            help: "DMM fallback found no usable cached result for the title variants tried."
                .to_string(),
        },
        "auto_acquire_no_provider_configured" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "No provider configured".to_string(),
            help: "Search-missing was enabled, but no acquisition provider is configured."
                .to_string(),
        },
        "auto_acquire_dry_run_preview" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Dry-run acquire preview".to_string(),
            help: "A release was found, but dry-run mode stopped before queue submission."
                .to_string(),
        },
        "auto_acquire_submit_failed" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Submit failed".to_string(),
            help: "A usable release was found, but submission to the acquisition backend failed."
                .to_string(),
        },
        "auto_acquire_internal_error" => SkipReasonPresentation {
            group: SkipReasonGroupKind::AutoAcquire,
            label: "Internal auto-acquire error".to_string(),
            help: "An unexpected auto-acquire error interrupted the request.".to_string(),
        },
        _ => {
            let group = skip_reason_group_kind(reason);
            let help = match group {
                SkipReasonGroupKind::Matcher => {
                    "Matcher skipped the item at this stage.".to_string()
                }
                SkipReasonGroupKind::Linking => {
                    "Link creation or update was skipped for this reason.".to_string()
                }
                SkipReasonGroupKind::Cleanup => {
                    "Cleanup skipped or flagged work for this reason.".to_string()
                }
                SkipReasonGroupKind::AutoAcquire => {
                    "Auto-acquire recorded this request outcome.".to_string()
                }
            };
            SkipReasonPresentation {
                group,
                label: humanize_skip_reason(reason),
                help,
            }
        }
    }
}

pub(crate) fn skip_reason_label(reason: &str) -> String {
    skip_reason_presentation(reason).label
}

pub(crate) fn skip_reason_group_label(reason: &str) -> String {
    skip_reason_presentation(reason).group.label().to_string()
}

pub(crate) fn skip_reason_help(reason: &str) -> String {
    skip_reason_presentation(reason).help
}

impl SkipReasonView {
    pub(super) fn from_reason(reason: String, count: i64) -> Self {
        let presentation = skip_reason_presentation(&reason);
        Self {
            reason,
            label: presentation.label,
            group: presentation.group.label().to_string(),
            help: presentation.help,
            count,
        }
    }
}

pub(crate) fn build_skip_reason_groups(
    skip_reasons: &[SkipReasonView],
) -> Vec<SkipReasonGroupView> {
    let group_order = [
        SkipReasonGroupKind::Matcher,
        SkipReasonGroupKind::Linking,
        SkipReasonGroupKind::Cleanup,
        SkipReasonGroupKind::AutoAcquire,
    ];
    let mut groups = Vec::new();

    for group_kind in group_order {
        let reasons = skip_reasons
            .iter()
            .filter(|reason| skip_reason_group_kind(&reason.reason) == group_kind)
            .cloned()
            .collect::<Vec<_>>();
        if reasons.is_empty() {
            continue;
        }
        groups.push(SkipReasonGroupView {
            group: group_kind.label().to_string(),
            total: reasons.iter().map(|reason| reason.count).sum(),
            reasons,
        });
    }

    groups
}
