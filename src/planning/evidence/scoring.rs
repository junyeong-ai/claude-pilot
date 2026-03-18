use super::types::{
    EvidenceItem, EvidenceQualitySummary, EvidenceSufficiency, QualityAssessment, QualityTier,
};
use super::Evidence;
use crate::config::{EvidenceBudgetConfig, QualityConfig};
use crate::utils::estimate_tokens;

impl Evidence {
    pub fn validate_quality(&self, config: &QualityConfig) -> EvidenceQualitySummary {
        let mut total_items = 0;
        let mut verifiable = 0;
        let mut confidence_sum = 0.0;
        let mut source_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut low_confidence = Vec::new();

        fn process_item<T: EvidenceItem>(
            item: &T,
            threshold: f32,
            total: &mut usize,
            verifiable: &mut usize,
            confidence_sum: &mut f32,
            source_counts: &mut std::collections::HashMap<String, usize>,
            low_confidence: &mut Vec<String>,
        ) {
            *total += 1;
            if item.source().is_verifiable() {
                *verifiable += 1;
            }
            *confidence_sum += item.confidence();
            if item.confidence() < threshold {
                low_confidence.push(format!(
                    "{} (confidence: {:.2})",
                    item.display_name(),
                    item.confidence()
                ));
            }
            *source_counts
                .entry(item.source().description())
                .or_insert(0) += 1;
        }

        for item in &self.codebase_analysis.relevant_files {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }
        for item in &self.prior_knowledge.relevant_skills {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }
        for item in &self.prior_knowledge.relevant_rules {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }

        let average_confidence = if total_items > 0 {
            confidence_sum / total_items as f32
        } else {
            0.0
        };
        let verifiable_ratio = if total_items > 0 {
            verifiable as f32 / total_items as f32
        } else {
            0.0
        };

        let semantic_source_count = self
            .codebase_analysis
            .relevant_files
            .iter()
            .filter(|f| f.source.is_semantic())
            .count();
        let semantic_ratio = if total_items > 0 {
            semantic_source_count as f32 / total_items as f32
        } else {
            0.0
        };

        let base_score = (verifiable_ratio * config.verifiable_weight)
            + (average_confidence * config.confidence_weight);
        let semantic_bonus = semantic_ratio * 0.1;
        let quality_score = (base_score + semantic_bonus).min(1.0);

        let mut source_breakdown: Vec<(String, usize)> = source_counts.into_iter().collect();
        source_breakdown.sort_by(|a, b| b.1.cmp(&a.1));

        EvidenceQualitySummary {
            total_evidence_items: total_items,
            verifiable_items: verifiable,
            average_confidence,
            source_breakdown,
            low_confidence_items: low_confidence,
            quality_score,
        }
    }

    pub fn quality_tier(&self, config: &QualityConfig) -> QualityAssessment {
        let summary = self.validate_quality(config);

        let yellow_quality = config.min_evidence_quality * 0.7;
        let yellow_confidence = config.min_evidence_confidence * 0.7;

        let tier = if summary.quality_score >= config.min_evidence_quality
            && summary.average_confidence >= config.min_evidence_confidence
        {
            QualityTier::Green
        } else if summary.quality_score >= yellow_quality
            && summary.average_confidence >= yellow_confidence
        {
            QualityTier::Yellow
        } else {
            QualityTier::Red
        };

        let has_verifiable = summary.verifiable_items > 0;

        QualityAssessment {
            tier,
            quality_score: summary.quality_score,
            confidence: summary.average_confidence,
            verifiable_items: summary.verifiable_items,
            total_items: summary.total_evidence_items,
            has_verifiable,
        }
    }

    pub fn validate_sufficiency(&self, config: &QualityConfig) -> EvidenceSufficiency {
        let summary = self.validate_quality(config);
        let mut missing = Vec::new();

        let mut coverage_factors = 0;
        let mut satisfied_factors = 0;

        coverage_factors += 1;
        if summary.quality_score >= config.min_evidence_quality {
            satisfied_factors += 1;
        } else {
            missing.push(format!(
                "quality score {:.0}% < {:.0}%",
                summary.quality_score * 100.0,
                config.min_evidence_quality * 100.0
            ));
        }

        coverage_factors += 1;
        if summary.average_confidence >= config.min_evidence_confidence {
            satisfied_factors += 1;
        } else {
            missing.push(format!(
                "avg confidence {:.0}% < {:.0}%",
                summary.average_confidence * 100.0,
                config.min_evidence_confidence * 100.0
            ));
        }

        if config.require_verifiable_evidence {
            coverage_factors += 1;
            if summary.verifiable_items > 0 {
                satisfied_factors += 1;
            } else {
                missing.push("no verifiable evidence".to_string());
            }
        }

        coverage_factors += 1;
        if !self.codebase_analysis.relevant_files.is_empty() {
            satisfied_factors += 1;
        } else {
            missing.push("no relevant files found".to_string());
        }

        coverage_factors += 1;
        if self.completeness.is_complete {
            satisfied_factors += 1;
        } else if !self.completeness.warnings.is_empty() {
            missing.push(format!(
                "truncated: {}",
                self.completeness.warnings.first().unwrap_or(&String::new())
            ));
        }

        let coverage = satisfied_factors as f32 / coverage_factors as f32;

        if missing.is_empty() && coverage >= 1.0 {
            EvidenceSufficiency::Complete
        } else if coverage >= config.evidence_coverage_threshold {
            EvidenceSufficiency::Sufficient { coverage, missing }
        } else if coverage >= config.risky_coverage_threshold {
            let risk_context = if missing.is_empty() {
                format!(
                    "Coverage {:.0}% is between {:.0}%-{:.0}% risky zone",
                    coverage * 100.0,
                    config.risky_coverage_threshold * 100.0,
                    config.evidence_coverage_threshold * 100.0
                )
            } else {
                format!(
                    "Coverage {:.0}% with gaps: {}",
                    coverage * 100.0,
                    missing.join(", ")
                )
            };
            EvidenceSufficiency::SufficientButRisky {
                coverage,
                missing,
                risk_context,
            }
        } else {
            let reason = if missing.is_empty() {
                format!(
                    "coverage {:.0}% below minimum {:.0}%",
                    coverage * 100.0,
                    config.risky_coverage_threshold * 100.0
                )
            } else {
                missing.join("; ")
            };
            EvidenceSufficiency::Insufficient { reason, coverage }
        }
    }

    pub fn to_markdown(&self, quality_config: &QualityConfig) -> String {
        let mut output = String::new();
        output.push_str("# Evidence Report\n\n");
        output.push_str(&format!(
            "**Gathered At**: {}\n\n",
            self.gathered_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        let quality = self.validate_quality(quality_config);
        output.push_str("## Evidence Quality\n\n");
        output.push_str(&format!(
            "- **Quality Score**: {:.1}%\n",
            quality.quality_score * 100.0
        ));
        output.push_str(&format!(
            "- **Total Items**: {}\n",
            quality.total_evidence_items
        ));
        output.push_str(&format!(
            "- **Verifiable**: {} ({:.0}%)\n",
            quality.verifiable_items,
            if quality.total_evidence_items > 0 {
                (quality.verifiable_items as f32 / quality.total_evidence_items as f32) * 100.0
            } else {
                0.0
            }
        ));
        output.push_str(&format!(
            "- **Avg Confidence**: {:.0}%\n\n",
            quality.average_confidence * 100.0
        ));

        if !quality.source_breakdown.is_empty() {
            output.push_str("### Evidence Sources\n\n");
            for (source, count) in &quality.source_breakdown {
                output.push_str(&format!("- {}: {} items\n", source, count));
            }
            output.push('\n');
        }

        if !quality.low_confidence_items.is_empty() {
            output.push_str("### Low Confidence Items\n\n");
            for item in &quality.low_confidence_items {
                output.push_str(&format!("- {}\n", item));
            }
            output.push('\n');
        }

        output.push_str("## Codebase Analysis\n\n");
        output.push_str("### Relevant Files\n\n");
        for file in &self.codebase_analysis.relevant_files {
            output.push_str(&format!(
                "- `{}` - {} [{}]\n",
                file.path,
                file.relevance,
                file.source.format_confidence_for_llm()
            ));
        }

        output.push_str("\n### Existing Patterns\n\n");
        for pattern in &self.codebase_analysis.existing_patterns {
            output.push_str(&format!("- {}\n", pattern));
        }

        output.push_str("\n### Conventions\n\n");
        for convention in &self.codebase_analysis.conventions {
            output.push_str(&format!("- {}\n", convention));
        }

        output.push_str("\n### Affected Areas\n\n");
        for area in &self.codebase_analysis.affected_areas {
            output.push_str(&format!("- {}\n", area));
        }

        output.push_str("\n## Dependency Analysis\n\n");
        output.push_str("### Current Dependencies\n\n");
        for dep in &self.dependency_analysis.current_dependencies {
            output.push_str(&format!("- {} ({})\n", dep.name, dep.version));
        }

        if !self.dependency_analysis.suggested_additions.is_empty() {
            output.push_str("\n### Suggested Additions\n\n");
            for dep in &self.dependency_analysis.suggested_additions {
                output.push_str(&format!("- {} - {}\n", dep.name, dep.reason));
            }
        }

        output.push_str("\n## Prior Knowledge\n\n");
        if !self.prior_knowledge.relevant_skills.is_empty() {
            output.push_str("### Relevant Skills\n\n");
            for skill in &self.prior_knowledge.relevant_skills {
                output.push_str(&format!(
                    "- {} - {} (confidence: {:.0}%)\n",
                    skill.name,
                    skill.relevance,
                    skill.confidence * 100.0
                ));
            }
        }

        if !self.prior_knowledge.relevant_rules.is_empty() {
            output.push_str("\n### Relevant Rules\n\n");
            for rule in &self.prior_knowledge.relevant_rules {
                output.push_str(&format!(
                    "- {} - {} (confidence: {:.0}%)\n",
                    rule.name,
                    rule.relevance,
                    rule.confidence * 100.0
                ));
            }
        }

        if !self.prior_knowledge.similar_patterns.is_empty() {
            output.push_str("\n### Similar Patterns\n\n");
            for pattern in &self.prior_knowledge.similar_patterns {
                output.push_str(&format!("- {}\n", pattern));
            }
        }

        output
    }

    pub fn to_markdown_with_budget(
        &self,
        max_tokens: usize,
        evidence_config: &EvidenceBudgetConfig,
        quality_config: &QualityConfig,
    ) -> String {
        let mut output = String::new();
        output.push_str("# Evidence Summary\n\n");

        let quality = self.validate_quality(quality_config);
        output.push_str(&format!(
            "Quality: {:.0}% | Items: {} | Confidence: {:.0}%\n\n",
            quality.quality_score * 100.0,
            quality.total_evidence_items,
            quality.average_confidence * 100.0
        ));

        let header_tokens = estimate_tokens(&output);
        let remaining_budget = max_tokens.saturating_sub(header_tokens);

        if remaining_budget < evidence_config.min_budget_threshold {
            output.push_str("_(Evidence truncated due to token limit)_\n");
            return output;
        }

        let file_budget = remaining_budget * evidence_config.file_budget_percent / 100;
        let pattern_budget = remaining_budget * evidence_config.pattern_budget_percent / 100;
        let dep_budget = remaining_budget * evidence_config.dependency_budget_percent / 100;
        let knowledge_budget = remaining_budget * evidence_config.knowledge_budget_percent / 100;

        let mut sorted_files: Vec<_> = self.codebase_analysis.relevant_files.iter().collect();
        sorted_files.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        output.push_str("## Key Files\n\n");
        let mut file_tokens = 0;
        for file in sorted_files {
            let category = file.source.confidence_category();
            let line = format!("- `{}` [{}]\n", file.path, category.label());
            let line_tokens = estimate_tokens(&line);
            if file_tokens + line_tokens > file_budget {
                output.push_str("- _(more files omitted)_\n");
                break;
            }
            output.push_str(&line);
            file_tokens += line_tokens;
        }

        output.push_str("\n## Patterns\n\n");
        let mut pattern_tokens = 0;
        for pattern in &self.codebase_analysis.existing_patterns {
            let line = format!("- {}\n", pattern);
            let line_tokens = estimate_tokens(&line);
            if pattern_tokens + line_tokens > pattern_budget / 2 {
                break;
            }
            output.push_str(&line);
            pattern_tokens += line_tokens;
        }
        for conv in &self.codebase_analysis.conventions {
            let line = format!("- {}\n", conv);
            let line_tokens = estimate_tokens(&line);
            if pattern_tokens + line_tokens > pattern_budget {
                break;
            }
            output.push_str(&line);
            pattern_tokens += line_tokens;
        }

        if !self.dependency_analysis.current_dependencies.is_empty() {
            output.push_str("\n## Dependencies\n\n");
            let mut dep_tokens = 0;
            for dep in self
                .dependency_analysis
                .current_dependencies
                .iter()
                .take(evidence_config.max_dependencies_display)
            {
                let line = format!("- {}\n", dep.name);
                let line_tokens = estimate_tokens(&line);
                if dep_tokens + line_tokens > dep_budget {
                    break;
                }
                output.push_str(&line);
                dep_tokens += line_tokens;
            }
        }

        let has_knowledge = !self.prior_knowledge.relevant_skills.is_empty()
            || !self.prior_knowledge.relevant_rules.is_empty();
        if has_knowledge {
            output.push_str("\n## Prior Knowledge\n\n");
            let mut know_tokens = 0;

            let mut skills: Vec<_> = self.prior_knowledge.relevant_skills.iter().collect();
            skills.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for skill in skills {
                if skill.confidence < evidence_config.min_confidence_display {
                    continue;
                }
                let line = format!(
                    "- Skill: {} ({:.0}%)\n",
                    skill.name,
                    skill.confidence * 100.0
                );
                let line_tokens = estimate_tokens(&line);
                if know_tokens + line_tokens > knowledge_budget / 2 {
                    break;
                }
                output.push_str(&line);
                know_tokens += line_tokens;
            }

            let mut rules: Vec<_> = self.prior_knowledge.relevant_rules.iter().collect();
            rules.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for rule in rules {
                if rule.confidence < evidence_config.min_confidence_display {
                    continue;
                }
                let line = format!("- Rule: {} ({:.0}%)\n", rule.name, rule.confidence * 100.0);
                let line_tokens = estimate_tokens(&line);
                if know_tokens + line_tokens > knowledge_budget {
                    break;
                }
                output.push_str(&line);
                know_tokens += line_tokens;
            }
        }

        let final_tokens = estimate_tokens(&output);
        if final_tokens > max_tokens {
            let target_len = max_tokens * evidence_config.token_char_ratio;
            if output.len() > target_len {
                output.truncate(
                    target_len.saturating_sub(evidence_config.markdown_truncation_buffer),
                );
                output.push_str("\n\n_(truncated)_");
            }
        }

        output
    }

    pub fn exceeds_budget(&self, max_tokens: usize, quality_config: &QualityConfig) -> bool {
        estimate_tokens(&self.to_markdown(quality_config)) > max_tokens
    }

    pub fn estimated_tokens(&self, quality_config: &QualityConfig) -> usize {
        estimate_tokens(&self.to_markdown(quality_config))
    }
}
