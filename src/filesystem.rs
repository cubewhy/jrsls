use std::path::PathBuf;

pub fn collect_files_with_ext(root: PathBuf, ext: &str) -> Vec<PathBuf> {
    let mut stack = vec![root];
    let mut results = Vec::new();

    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if path
                .extension()
                .and_then(|s| s.to_str())
                .map(|name| name.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
            {
                results.push(path);
            }
        }
    }

    results
}
