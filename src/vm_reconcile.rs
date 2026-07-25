use roxmltree::{Document, Node};

pub(crate) fn merge_disk_xml(
    current_xml: &str,
    current_disk: Node<'_, '_>,
    desired_xml: &str,
) -> String {
    let desired_doc = Document::parse(desired_xml).expect("generated disk XML should parse");
    let desired_disk = desired_doc.root_element();
    let desired_driver = desired_child(desired_disk, "driver");
    let desired_source = desired_child(desired_disk, "source");
    let desired_target = desired_child(desired_disk, "target");
    let desired_alias = desired_disk
        .children()
        .find(|child| child.has_tag_name("alias"));

    let mut output = render_element_start(
        "    ",
        desired_disk,
        Some(current_disk),
        &["type", "device"],
    );
    output.push_str(">\n");

    let mut emitted_driver = !current_disk
        .children()
        .any(|child| child.has_tag_name("driver"));
    if emitted_driver {
        output.push_str(&render_disk_child(
            current_xml,
            desired_xml,
            desired_driver,
            None,
        ));
    }
    let mut emitted_source = false;
    let mut emitted_target = false;
    let mut emitted_alias = false;
    for child in current_disk.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "driver" if !emitted_driver => {
                output.push_str(&render_disk_child(
                    current_xml,
                    desired_xml,
                    desired_driver,
                    Some(child),
                ));
                emitted_driver = true;
            }
            "source" if !emitted_source => {
                output.push_str(&render_disk_child(
                    current_xml,
                    desired_xml,
                    desired_source,
                    Some(child),
                ));
                emitted_source = true;
            }
            "target" if !emitted_target => {
                output.push_str(&render_disk_child(
                    current_xml,
                    desired_xml,
                    desired_target,
                    Some(child),
                ));
                emitted_target = true;
            }
            "alias" if !emitted_alias => {
                if is_qtr_disk_alias(child)
                    && let Some(desired_alias) = desired_alias
                {
                    output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_alias = true;
            }
            "address" if !emitted_alias => {
                if let Some(desired_alias) = desired_alias {
                    output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
                }
                emitted_alias = true;
                output.push_str(&render_raw_node(current_xml, child, "      "));
            }
            "driver" | "source" | "target" => {}
            _ => output.push_str(&render_raw_node(current_xml, child, "      ")),
        }
    }

    for (emitted, desired) in [
        (emitted_driver, desired_driver),
        (emitted_source, desired_source),
        (emitted_target, desired_target),
    ] {
        if !emitted {
            output.push_str(&render_disk_child(current_xml, desired_xml, desired, None));
        }
    }
    if !emitted_alias && let Some(desired_alias) = desired_alias {
        output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
    }
    output.push_str("    </disk>\n");
    output
}

pub(crate) fn merge_cdrom_xml(
    current_xml: &str,
    current_cdrom: Node<'_, '_>,
    desired_xml: &str,
) -> String {
    let desired_doc = Document::parse(desired_xml).expect("generated CD-ROM XML should parse");
    let desired_cdrom = desired_doc.root_element();
    let desired_driver = desired_child(desired_cdrom, "driver");
    let desired_source = desired_cdrom
        .children()
        .find(|child| child.has_tag_name("source"));
    let desired_target = desired_child(desired_cdrom, "target");
    let desired_readonly = desired_child(desired_cdrom, "readonly");
    let desired_alias = desired_child(desired_cdrom, "alias");

    let mut output = render_element_start(
        "    ",
        desired_cdrom,
        Some(current_cdrom),
        &["type", "device"],
    );
    output.push_str(">\n");

    let mut emitted_driver = !current_cdrom
        .children()
        .any(|child| child.has_tag_name("driver"));
    if emitted_driver {
        output.push_str(&render_disk_child(
            current_xml,
            desired_xml,
            desired_driver,
            None,
        ));
    }
    let mut emitted_source = false;
    let mut emitted_target = false;
    let mut emitted_readonly = false;
    let mut emitted_alias = false;
    for child in current_cdrom.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "driver" if !emitted_driver => {
                output.push_str(&render_disk_child(
                    current_xml,
                    desired_xml,
                    desired_driver,
                    Some(child),
                ));
                emitted_driver = true;
            }
            "source" if !emitted_source => {
                if let Some(desired_source) = desired_source {
                    output.push_str(&render_disk_child(
                        current_xml,
                        desired_xml,
                        desired_source,
                        Some(child),
                    ));
                }
                emitted_source = true;
            }
            "target" if !emitted_target => {
                if !emitted_source && let Some(desired_source) = desired_source {
                    output.push_str(&render_disk_child(
                        current_xml,
                        desired_xml,
                        desired_source,
                        None,
                    ));
                    emitted_source = true;
                }
                output.push_str(&render_disk_child(
                    current_xml,
                    desired_xml,
                    desired_target,
                    Some(child),
                ));
                emitted_target = true;
            }
            "readonly" if !emitted_readonly => {
                output.push_str(&render_raw_node(desired_xml, desired_readonly, "      "));
                emitted_readonly = true;
            }
            "alias" if !emitted_alias => {
                if is_qtr_alias(child, "ua-qtr-cdrom-") {
                    output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_alias = true;
            }
            "address" => {
                if !emitted_readonly {
                    output.push_str(&render_raw_node(desired_xml, desired_readonly, "      "));
                    emitted_readonly = true;
                }
                if !emitted_alias {
                    output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
                    emitted_alias = true;
                }
                output.push_str(&render_raw_node(current_xml, child, "      "));
            }
            "driver" | "source" | "target" | "readonly" | "alias" => {}
            _ => output.push_str(&render_raw_node(current_xml, child, "      ")),
        }
    }

    if !emitted_driver {
        output.push_str(&render_disk_child(
            current_xml,
            desired_xml,
            desired_driver,
            None,
        ));
    }
    if !emitted_source && let Some(desired_source) = desired_source {
        output.push_str(&render_disk_child(
            current_xml,
            desired_xml,
            desired_source,
            None,
        ));
    }
    if !emitted_target {
        output.push_str(&render_disk_child(
            current_xml,
            desired_xml,
            desired_target,
            None,
        ));
    }
    if !emitted_readonly {
        output.push_str(&render_raw_node(desired_xml, desired_readonly, "      "));
    }
    if !emitted_alias {
        output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
    }
    output.push_str("    </disk>\n");
    output
}

fn is_qtr_disk_alias(alias: Node<'_, '_>) -> bool {
    is_qtr_alias(alias, "ua-qtr-disk-")
}

fn is_qtr_alias(alias: Node<'_, '_>, prefix: &str) -> bool {
    alias
        .attribute("name")
        .is_some_and(|name| name.starts_with(prefix))
}

fn desired_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Node<'a, 'input> {
    node.children()
        .find(|child| child.has_tag_name(tag))
        .unwrap_or_else(|| panic!("generated disk XML should contain {tag}"))
}

fn render_disk_child(
    current_xml: &str,
    desired_xml: &str,
    desired: Node<'_, '_>,
    current: Option<Node<'_, '_>>,
) -> String {
    let (managed_attributes, managed_children): (&[&str], &[&str]) = match desired.tag_name().name()
    {
        "driver" => (&["name", "type", "cache", "io", "queues"], &["iothreads"]),
        "source" => (&["file", "dev"], &[]),
        "target" => (&["dev", "bus"], &[]),
        _ => unreachable!("unexpected managed disk child"),
    };
    let mut output = render_element_start("      ", desired, current, managed_attributes);
    let desired_managed_children = desired
        .children()
        .filter(Node::is_element)
        .filter(|child| managed_children.contains(&child.tag_name().name()))
        .collect::<Vec<_>>();
    let current_opaque_children = current
        .into_iter()
        .flat_map(|node| node.children())
        .filter(Node::is_element)
        .filter(|child| !managed_children.contains(&child.tag_name().name()))
        .collect::<Vec<_>>();

    if desired_managed_children.is_empty() && current_opaque_children.is_empty() {
        output.push_str("/>\n");
        return output;
    }

    output.push_str(">\n");
    for child in desired_managed_children {
        output.push_str(&render_raw_node(desired_xml, child, "        "));
    }
    for child in current_opaque_children {
        output.push_str(&render_raw_node(current_xml, child, "        "));
    }
    output.push_str(&format!("      </{}>\n", desired.tag_name().name()));
    output
}

fn render_element_start(
    indent: &str,
    desired: Node<'_, '_>,
    current: Option<Node<'_, '_>>,
    managed_attributes: &[&str],
) -> String {
    let mut output = format!("{indent}<{}", desired.tag_name().name());
    for attribute in desired.attributes() {
        output.push_str(&format!(
            " {}='{}'",
            attribute.name(),
            escape_xml(attribute.value())
        ));
    }
    if let Some(current) = current {
        for attribute in current.attributes().filter(|attribute| {
            !managed_attributes.contains(&attribute.name())
                && desired.attribute(attribute.name()).is_none()
        }) {
            output.push_str(&format!(
                " {}='{}'",
                attribute.name(),
                escape_xml(attribute.value())
            ));
        }
    }
    output
}

fn render_raw_node(xml: &str, node: Node<'_, '_>, indent: &str) -> String {
    format!("{indent}{}\n", &xml[node.range()])
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
