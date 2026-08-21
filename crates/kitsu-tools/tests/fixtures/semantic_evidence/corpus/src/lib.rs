mod slug;

pub fn product_path(name: &str) -> String {
    format!("/products/{}", slug::normalize_slug(name))
}
