use std::path::Path;

pub struct RulesLoader;

impl RulesLoader {
    pub fn load_for_file(file_path: &Path) -> Vec<String> {
        let mut rules = Vec::new();

        if let Some(root) = Self::find_repo_root(file_path) {
            let repo_rules = root.join(".charm").join("rules.md");
            if repo_rules.exists()
                && let Ok(content) = std::fs::read_to_string(&repo_rules)
            {
                rules.push(format!(
                    "## Repo Rules ({}):\n{}",
                    repo_rules.display(),
                    content
                ));
            }
        }

        let mut current = file_path.parent();
        while let Some(dir) = current {
            let dir_rules = dir.join(".charm-rules.md");
            if dir_rules.exists()
                && let Ok(content) = std::fs::read_to_string(&dir_rules)
            {
                rules.push(format!("## Dir Rules ({}):\n{}", dir.display(), content));
            }
            current = dir.parent();
        }

        rules
    }

    pub fn load_all(workspace_root: &Path) -> Vec<String> {
        let mut rules = Vec::new();

        let repo_rules = workspace_root.join(".charm").join("rules.md");
        if repo_rules.exists()
            && let Ok(content) = std::fs::read_to_string(&repo_rules)
        {
            rules.push(content);
        }

        for entry in walkdir::WalkDir::new(workspace_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name() == ".charm-rules.md"
                && let Ok(content) = std::fs::read_to_string(entry.path())
            {
                rules.push(content);
            }
        }

        rules
    }

    fn find_repo_root(start: &Path) -> Option<&Path> {
        let mut current = start.parent();
        while let Some(dir) = current {
            if dir.join(".git").exists() || dir.join(".charm").exists() {
                return Some(dir);
            }
            current = dir.parent();
        }
        None
    }
}
