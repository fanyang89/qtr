use comfy_table::{Table, TableComponent, presets::NOTHING};

pub fn print_table(headers: &[&str], rows: impl IntoIterator<Item = Vec<String>>) {
    let mut table = Table::new();
    table
        .load_preset(NOTHING)
        .set_style(TableComponent::HeaderLines, '═')
        .set_style(TableComponent::MiddleHeaderIntersections, '═')
        .set_header(headers.to_vec());

    for row in rows {
        table.add_row(row);
    }

    println!("{table}");
}
