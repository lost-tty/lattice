
fn reconstruct_args(args: &[String]) -> String {
    args.iter().map(|arg| {
        // Detect trailing closing parentheses which are likely structural
        let suffix_start = arg.rfind(|c| c != ')').map(|i| i + 1).unwrap_or(0);
        // Handle case where string is all parens ")))"
        if suffix_start == 0 && arg.chars().all(|c| c == ')') {
             if arg.is_empty() { return "\"\"".to_string(); } // Was empty string
             return arg.clone(); 
        }
        
        let (stem, suffix) = arg.split_at(suffix_start);
        
        // Quote stem if it has spaces or is empty (and we have no suffix or explicit empty)
        if stem.contains(' ') || stem.contains('\t') || stem.contains('\n') || stem.is_empty() {
             format!("\"{}\"{}", stem.replace('\"', "\\\""), suffix)
        } else {
             arg.clone()
        }
    }).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconstruct_args() {
        // Case 1: unquoted
        let args = vec!["((put".to_string(), "c".to_string(), "foo))".to_string()];
        assert_eq!(reconstruct_args(&args), "((put c foo))");

        // Case 2: spaces (shlex stripped quotes)
        // User typed: ((put c "Hello world"))
        // Shlex gave: ["((put", "c", "Hello world))"]  <-- Wait, shlex splits by space unless quoted.
        // Actually, "Hello world" -> Hello world.
        // So args are: ["((put", "c", "Hello", "world))"] ??
        // If user typed `((put c "Hello world"))`
        // shlex sees: `((put`, `c`, `"Hello world"` (quoted), `))`
        // If they are adjacent? `((put c "Hello world"))` -> `((put`, `c`, `Hello world))` tokens. (Because shlex handles adjacent quotes).
        // So args is ["((put", "c", "Hello world))"]
        let args = vec!["((put".to_string(), "c".to_string(), "Hello world))".to_string()];
        // We expect: ((put c "Hello world))")
        // lexpr will parse as: ((put c "Hello world))")) -- String("Hello world))").
        // This is STILL BROKEN for S-expressions if user doesn't space out the closing parens.
        // BUT, at least it doesn't fail with "quote".
        assert_eq!(reconstruct_args(&args), "((put c \"Hello world))\")");
        
        // Case 3: Proper spacing
        // User typed: ((put c "Hello world" ))
        // Shlex: ["((put", "c", "Hello world", "))"]
        let args = vec!["((put".to_string(), "c".to_string(), "Hello world".to_string(), "))".to_string()];
        assert_eq!(reconstruct_args(&args), "((put c \"Hello world\" ))");
        
        // Case 4: No spaces, tokens with parens
        // User typed: ((put c x))
        // Shlex: ["((put", "c", "x))"]
        let args = vec!["((put".to_string(), "c".to_string(), "x))".to_string()];
        assert_eq!(reconstruct_args(&args), "((put c x))");
        // This confirms fixing the "quote" error for case 4.
    }
}
