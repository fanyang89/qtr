use roxmltree::{Document, Node};

pub(crate) fn merge_interface_xml(
    current_xml: &str,
    current: Node<'_, '_>,
    desired_xml: &str,
    manage_mac: bool,
) -> String {
    let desired_doc = Document::parse(desired_xml).expect("generated interface XML should parse");
    let desired = desired_doc.root_element();
    let desired_mac = desired.children().find(|child| child.has_tag_name("mac"));
    let desired_source = desired_child(desired, "source");
    let desired_model = desired_child(desired, "model");
    let desired_alias = desired.children().find(|child| child.has_tag_name("alias"));
    let mut output = render_element_start("    ", desired, Some(current), &["type"]);
    output.push_str(">\n");
    let mut emitted_mac = false;
    let mut emitted_source = false;
    let mut emitted_model = false;
    let mut emitted_alias = false;

    for child in current.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "mac" if !emitted_mac => {
                if manage_mac {
                    if let Some(mac) = desired_mac {
                        output.push_str(&render_interface_child(
                            current_xml,
                            mac,
                            Some(child),
                            &["address"],
                        ));
                    }
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_mac = true;
            }
            "source" if !emitted_source => {
                output.push_str(&render_interface_child(
                    current_xml,
                    desired_source,
                    Some(child),
                    &["network", "bridge"],
                ));
                emitted_source = true;
            }
            "model" if !emitted_model => {
                output.push_str(&render_interface_child(
                    current_xml,
                    desired_model,
                    Some(child),
                    &["type"],
                ));
                emitted_model = true;
            }
            "alias" if !emitted_alias => {
                emit_missing_interface_children(
                    &mut output,
                    desired_xml,
                    desired_mac,
                    desired_source,
                    desired_model,
                    manage_mac,
                    &mut emitted_mac,
                    &mut emitted_source,
                    &mut emitted_model,
                );
                if is_qtr_alias(child, "ua-qtr-nic-") {
                    if let Some(alias) = desired_alias {
                        output.push_str(&render_raw_node(desired_xml, alias, "      "));
                    }
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_alias = true;
            }
            "address" if !emitted_alias => {
                emit_missing_interface_children(
                    &mut output,
                    desired_xml,
                    desired_mac,
                    desired_source,
                    desired_model,
                    manage_mac,
                    &mut emitted_mac,
                    &mut emitted_source,
                    &mut emitted_model,
                );
                if let Some(alias) = desired_alias {
                    output.push_str(&render_raw_node(desired_xml, alias, "      "));
                }
                emitted_alias = true;
                output.push_str(&render_raw_node(current_xml, child, "      "));
            }
            "mac" | "source" | "model" | "alias" => {}
            _ => output.push_str(&render_raw_node(current_xml, child, "      ")),
        }
    }
    emit_missing_interface_children(
        &mut output,
        desired_xml,
        desired_mac,
        desired_source,
        desired_model,
        manage_mac,
        &mut emitted_mac,
        &mut emitted_source,
        &mut emitted_model,
    );
    if !emitted_alias && let Some(alias) = desired_alias {
        output.push_str(&render_raw_node(desired_xml, alias, "      "));
    }
    output.push_str("    </interface>\n");
    output
}

#[allow(clippy::too_many_arguments)]
fn emit_missing_interface_children(
    output: &mut String,
    desired_xml: &str,
    mac: Option<Node<'_, '_>>,
    source: Node<'_, '_>,
    model: Node<'_, '_>,
    manage_mac: bool,
    emitted_mac: &mut bool,
    emitted_source: &mut bool,
    emitted_model: &mut bool,
) {
    if manage_mac && !*emitted_mac {
        if let Some(mac) = mac {
            output.push_str(&render_raw_node(desired_xml, mac, "      "));
        }
        *emitted_mac = true;
    }
    if !*emitted_source {
        output.push_str(&render_raw_node(desired_xml, source, "      "));
        *emitted_source = true;
    }
    if !*emitted_model {
        output.push_str(&render_raw_node(desired_xml, model, "      "));
        *emitted_model = true;
    }
}

fn render_interface_child(
    current_xml: &str,
    desired: Node<'_, '_>,
    current: Option<Node<'_, '_>>,
    managed_attributes: &[&str],
) -> String {
    let mut output = render_element_start("      ", desired, current, managed_attributes);
    let opaque = current
        .into_iter()
        .flat_map(|node| node.children())
        .filter(Node::is_element)
        .collect::<Vec<_>>();
    if opaque.is_empty() {
        output.push_str("/>\n");
    } else {
        output.push_str(">\n");
        for child in opaque {
            output.push_str(&render_raw_node(current_xml, child, "        "));
        }
        output.push_str(&format!("      </{}>\n", desired.tag_name().name()));
    }
    output
}

pub(crate) fn merge_disk_xml(
    current_xml: &str,
    current_disk: Node<'_, '_>,
    desired_xml: &str,
    manage_iotune: bool,
    manage_readonly: bool,
    manage_serial: bool,
) -> String {
    let desired_doc = Document::parse(desired_xml).expect("generated disk XML should parse");
    let desired_disk = desired_doc.root_element();
    let desired_driver = desired_child(desired_disk, "driver");
    let desired_source = desired_child(desired_disk, "source");
    let desired_target = desired_child(desired_disk, "target");
    let desired_iotune = desired_disk
        .children()
        .find(|child| child.has_tag_name("iotune"));
    let desired_readonly = desired_disk
        .children()
        .find(|child| child.has_tag_name("readonly"));
    let desired_serial = desired_disk
        .children()
        .find(|child| child.has_tag_name("serial"));
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
    let mut emitted_iotune = false;
    let mut emitted_readonly = false;
    let mut emitted_serial = false;
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
            "iotune" if !emitted_iotune => {
                if manage_iotune {
                    if let Some(desired_iotune) = desired_iotune {
                        output.push_str(&merge_iotune_xml(
                            current_xml,
                            child,
                            desired_xml,
                            desired_iotune,
                        ));
                    }
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_iotune = true;
            }
            "readonly" if !emitted_readonly => {
                if manage_readonly {
                    if let Some(desired_readonly) = desired_readonly {
                        output.push_str(&render_raw_node(desired_xml, desired_readonly, "      "));
                    }
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_readonly = true;
            }
            "serial" if !emitted_serial => {
                if manage_serial {
                    if let Some(desired_serial) = desired_serial {
                        output.push_str(&render_raw_node(desired_xml, desired_serial, "      "));
                    }
                } else {
                    output.push_str(&render_raw_node(current_xml, child, "      "));
                }
                emitted_serial = true;
            }
            "alias" if !emitted_alias => {
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_iotune,
                    manage_iotune,
                    &mut emitted_iotune,
                );
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_readonly,
                    manage_readonly,
                    &mut emitted_readonly,
                );
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_serial,
                    manage_serial,
                    &mut emitted_serial,
                );
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
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_iotune,
                    manage_iotune,
                    &mut emitted_iotune,
                );
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_readonly,
                    manage_readonly,
                    &mut emitted_readonly,
                );
                emit_optional_disk_child(
                    &mut output,
                    desired_xml,
                    desired_serial,
                    manage_serial,
                    &mut emitted_serial,
                );
                if let Some(desired_alias) = desired_alias {
                    output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
                }
                emitted_alias = true;
                output.push_str(&render_raw_node(current_xml, child, "      "));
            }
            "driver" | "source" | "target" | "iotune" | "readonly" | "serial" => {}
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
        emit_optional_disk_child(
            &mut output,
            desired_xml,
            desired_iotune,
            manage_iotune,
            &mut emitted_iotune,
        );
        emit_optional_disk_child(
            &mut output,
            desired_xml,
            desired_readonly,
            manage_readonly,
            &mut emitted_readonly,
        );
        emit_optional_disk_child(
            &mut output,
            desired_xml,
            desired_serial,
            manage_serial,
            &mut emitted_serial,
        );
        output.push_str(&render_raw_node(desired_xml, desired_alias, "      "));
    }
    emit_optional_disk_child(
        &mut output,
        desired_xml,
        desired_iotune,
        manage_iotune,
        &mut emitted_iotune,
    );
    emit_optional_disk_child(
        &mut output,
        desired_xml,
        desired_readonly,
        manage_readonly,
        &mut emitted_readonly,
    );
    emit_optional_disk_child(
        &mut output,
        desired_xml,
        desired_serial,
        manage_serial,
        &mut emitted_serial,
    );
    output.push_str("    </disk>\n");
    output
}

fn merge_iotune_xml(
    current_xml: &str,
    current: Node<'_, '_>,
    desired_xml: &str,
    desired: Node<'_, '_>,
) -> String {
    const MANAGED_CHILDREN: &[&str] = &[
        "total_bytes_sec",
        "read_bytes_sec",
        "write_bytes_sec",
        "total_iops_sec",
        "read_iops_sec",
        "write_iops_sec",
    ];

    let mut output = render_element_start("      ", desired, Some(current), &[]);
    output.push_str(">\n");
    for child in desired.children().filter(Node::is_element) {
        output.push_str(&render_raw_node(desired_xml, child, "        "));
    }
    for child in current
        .children()
        .filter(Node::is_element)
        .filter(|child| !MANAGED_CHILDREN.contains(&child.tag_name().name()))
    {
        output.push_str(&render_raw_node(current_xml, child, "        "));
    }
    output.push_str("      </iotune>\n");
    output
}

fn emit_optional_disk_child(
    output: &mut String,
    desired_xml: &str,
    desired: Option<Node<'_, '_>>,
    manage: bool,
    emitted: &mut bool,
) {
    if manage && !*emitted {
        if let Some(desired) = desired {
            output.push_str(&render_raw_node(desired_xml, desired, "      "));
        }
        *emitted = true;
    }
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
