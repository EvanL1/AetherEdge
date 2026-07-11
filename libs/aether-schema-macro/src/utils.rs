//! Utility functions for schema macro

use syn::{GenericArgument, PathArguments, Type, TypePath};

/// Extract the type name from a Type
///
/// # Examples
///
/// ```ignore
/// let ty: Type = parse_quote!(String);
/// assert_eq!(extract_type_name(&ty), "String");
///
/// let ty: Type = parse_quote!(Option<String>);
/// assert_eq!(extract_type_name(&ty), "Option");
/// ```
pub fn extract_type_name(ty: &Type) -> String {
    match ty {
        Type::Path(TypePath { path, .. }) => path
            .segments
            .last()
            .map(|seg| seg.ident.to_string())
            .unwrap_or_else(|| "Unknown".to_string()),
        _ => "Unknown".to_string(),
    }
}

/// Extract inner type from `Option<T>`
///
/// Returns `Some(&T)` if the type is `Option<T>`, None otherwise
pub fn extract_option_inner(ty: &Type) -> Option<&Type> {
    if let Type::Path(TypePath { path, .. }) = ty
        && let Some(segment) = path.segments.last()
        && segment.ident == "Option"
        && let PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(GenericArgument::Type(inner_ty)) = args.args.first()
    {
        return Some(inner_ty);
    }
    None
}

/// Check if type is `Vec<u8>`
pub fn is_u8_vec(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, .. }) = ty
        && let Some(segment) = path.segments.last()
        && segment.ident == "Vec"
        && let PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(GenericArgument::Type(Type::Path(inner_path))) = args.args.first()
        && let Some(inner_seg) = inner_path.path.segments.last()
    {
        return inner_seg.ident == "u8";
    }
    false
}

/// Check if type is a JSON type (HashMap, BTreeMap, Value, etc.)
pub fn is_json_type(ty: &Type) -> bool {
    let type_name = extract_type_name(ty);
    matches!(type_name.as_str(), "HashMap" | "BTreeMap" | "Value" | "Map")
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
#[allow(clippy::disallowed_methods)] // Test code - unwrap is acceptable
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_extract_type_name() {
        let ty: Type = parse_quote!(String);
        assert_eq!(extract_type_name(&ty), "String");

        let ty: Type = parse_quote!(u16);
        assert_eq!(extract_type_name(&ty), "u16");

        let ty: Type = parse_quote!(Option<String>);
        assert_eq!(extract_type_name(&ty), "Option");
    }

    #[test]
    fn test_extract_option_inner() {
        let ty: Type = parse_quote!(Option<String>);
        assert!(extract_option_inner(&ty).is_some());

        let ty: Type = parse_quote!(String);
        assert!(extract_option_inner(&ty).is_none());
    }

    #[test]
    fn test_is_u8_vec() {
        let ty: Type = parse_quote!(Vec<u8>);
        assert!(is_u8_vec(&ty));

        let ty: Type = parse_quote!(Vec<String>);
        assert!(!is_u8_vec(&ty));

        let ty: Type = parse_quote!(String);
        assert!(!is_u8_vec(&ty));
    }

    #[test]
    fn test_is_json_type() {
        let ty: Type = parse_quote!(HashMap<String, String>);
        assert!(is_json_type(&ty));

        let ty: Type = parse_quote!(Value);
        assert!(is_json_type(&ty));

        let ty: Type = parse_quote!(String);
        assert!(!is_json_type(&ty));
    }
}
