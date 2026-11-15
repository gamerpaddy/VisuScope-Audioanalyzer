//! Tests of `editor.rs`.
use super::*;

fn entry(kind: Kind) -> Entry {
    Entry { label: kind.name().into(), group: kind.info().group, keywords: String::new(), kind, s: String::new(), p0: None }
}

#[test]
fn new_nodes_link_up_and_split_wires() {
    let mut r = Routing::new();
    r.g = Graph::starter("dev", None);
    let (inp, ana) = (1, 2);
    // insert a node into the wire input -> analyzer
    r.add_entry(&entry(Kind::Eq), pos2(100.0, 0.0), Link::Insert(ana, 0));
    let eq = r.g.nodes.iter().find(|n| n.kind == Kind::Eq).unwrap().id;
    assert_eq!(r.g.edge_into(eq, 0).unwrap().from, inp);
    assert_eq!(r.g.edge_into(ana, 0).unwrap().from, eq);
    // dropping a wire from the input on empty canvas and picking a mixer connects it
    r.add_entry(&entry(Kind::Mixer), pos2(0.0, 100.0), Link::FromOut(inp, 0));
    let mx = r.g.nodes.iter().find(|n| n.kind == Kind::Mixer).unwrap().id;
    assert_eq!(r.g.edge_into(mx, 0).unwrap().from, inp);
    // the input now feeds two nodes: a split
    assert_eq!(r.g.edges.iter().filter(|e| e.from == inp).count(), 2);
    // a sink picked from a wire that starts at an input port ends up connected to its source
    r.add_entry(&entry(Kind::Generator), pos2(0.0, 200.0), Link::ToIn(mx, 1));
    let gen_id = r.g.nodes.iter().find(|n| n.kind == Kind::Generator).unwrap().id;
    assert_eq!(r.g.edge_into(mx, 1).unwrap().from, gen_id);
    // a node without an input cannot be inserted into a wire
    let before = r.g.edges.len();
    r.add_entry(&entry(Kind::Generator), pos2(0.0, 300.0), Link::Insert(ana, 0));
    assert_eq!(r.g.edges.len(), before);
}

#[test]
fn routings_can_be_saved_listed_loaded_and_names_are_cleaned() {
    let dir = std::env::temp_dir().join(format!("visuscope_routings_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY of the test: nothing else reads this variable
    unsafe { std::env::set_var("VISUSCOPE_ROUTINGS_DIR", &dir) };
    assert!(list_routings().is_empty());
    assert_eq!(clean_name("  ../evil/na:me*? "), "evilname");
    // device names, trailing dots and spaces, and very long names
    assert_eq!(clean_name("con"), "con_");
    assert_eq!(clean_name("NUL.txt"), "NUL.txt_");
    assert_eq!(clean_name("Lpt1"), "Lpt1_");
    assert_eq!(clean_name("COM10"), "COM10");
    assert_eq!(clean_name("name. . "), "name");
    assert_eq!(clean_name(&"x".repeat(300)).len(), 80);
    let mut g = Graph::starter("dev", None);
    let eq = g.add(Kind::Eq, 5.0, 6.0);
    g.node_mut(eq).unwrap().p[2] = 3.0;
    save_routing("Studio A", &g).unwrap();
    save_routing("alpha", &Graph::default()).unwrap();
    assert!(save_routing("///", &g).is_err());
    assert_eq!(list_routings(), vec!["alpha".to_string(), "Studio A".to_string()]);
    let back = load_routing("Studio A").unwrap();
    assert_eq!(back.nodes.len(), g.nodes.len());
    assert_eq!(back.node(eq).unwrap().p[2], 3.0);
    assert!(load_routing("missing").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nodes_do_not_land_on_each_other_and_overlaps_are_tidied() {
    let mut r = Routing::new();
    let a = r.g.add(Kind::Eq, 100.0, 100.0);
    let b = r.g.add(Kind::Comp, 120.0, 130.0);
    let c = r.g.add(Kind::Gain, 100.0, 100.0);
    for id in [a, b, c] {
        let n = r.g.node(id).unwrap().clone();
        r.sizes.insert(id, (node_w(n.kind), 180.0));
    }
    r.tidy();
    let rects: Vec<Rect> = [a, b, c].iter().map(|id| r.node_rect(r.g.node(*id).unwrap())).collect();
    for i in 0..3 {
        for j in i + 1..3 {
            assert!(!rects[i].intersects(rects[j]), "{:?} overlaps {:?}", rects[i], rects[j]);
        }
    }
    // a node added where something already is goes to a free place
    r.add_entry(&entry(Kind::Delay), rects[0].center(), Link::None);
    let d = r.g.nodes.last().unwrap().clone();
    let rd = Rect::from_min_size(pos2(d.x, d.y), vec2(node_w(Kind::Delay), 170.0));
    assert!(rects.iter().all(|x| !x.intersects(rd)), "the new node sits on another one");
}

#[test]
fn a_wire_dropped_between_two_close_dots_goes_to_the_nearer_one() {
    // the rows of a mixer at a small zoom: 14 pixels apart, the old radius of 15 reached both and took the first
    let c = [(1, 0u8, pos2(100.0, 100.0)), (1, 1, pos2(100.0, 114.0)), (1, 2, pos2(100.0, 128.0))];
    assert_eq!(pick_port(&c, pos2(103.0, 113.0), 7.0), Some((1, 1)));
    assert_eq!(pick_port(&c, pos2(103.0, 127.0), 7.0), Some((1, 2)));
    assert_eq!(pick_port(&c, pos2(103.0, 101.0), 7.0), Some((1, 0)));
    assert_eq!(pick_port(&c, pos2(140.0, 113.0), 7.0), None);
    assert_eq!(pick_port(&[], pos2(0.0, 0.0), 7.0), None);
}

#[test]
fn a_long_wire_can_be_hit_anywhere_along_its_length() {
    // 2500 pixels long: the old 21 sample points were 120 pixels apart
    let pts = wire_pts(pos2(0.0, 0.0), pos2(2500.0, 400.0));
    for k in 0..=200 {
        let on = bezier_at(&pts, k as f32 / 200.0);
        assert!(wire_distance(&pts, on + vec2(0.0, 3.0)) < 4.0, "point {k}");
    }
    assert!(wire_distance(&pts, pos2(1250.0, 200.0 + 80.0)) > 20.0, "a point far from the line is not on it");
    // a degenerate wire does not divide by zero
    let flat = wire_pts(pos2(5.0, 5.0), pos2(5.0, 5.0));
    assert!(wire_distance(&flat, pos2(5.0, 5.0)).is_finite());
}

#[test]
fn every_settled_change_is_one_undo_step_and_a_drag_is_only_one() {
    let mut g = Graph::default();
    let a = g.add(Kind::Generator, 0.0, 0.0);
    let mut h = History::default();
    h.track(&g, true);
    assert!(h.undo.is_empty());
    // a drag: many changes while the button is down, then released
    let f = g.add(Kind::Gain, 10.0, 10.0);
    g.connect(a, f, 0).unwrap();
    h.track(&g, true);
    assert_eq!(h.undo.len(), 1);
    for k in 0..50 {
        g.node_mut(f).unwrap().x = k as f32;
        h.track(&g, false);
    }
    assert_eq!(h.undo.len(), 1, "nothing is recorded while the button is down");
    h.track(&g, true);
    assert_eq!(h.undo.len(), 2);
    // delete the gain node (joined back up or not), undo brings it and its line back, redo removes it again
    let before = g.clone();
    g.remove_bridged(f);
    h.track(&g, true);
    assert!(g.node(f).is_none());
    let back = h.back(&g).unwrap();
    assert!(back.node(f).is_some() && back.edges.len() == before.edges.len());
    assert_eq!(back.node(f).unwrap().x, 49.0);
    let again = h.forward(&back).unwrap();
    assert!(again.node(f).is_none());
    // a new change after an undo ends the redo line
    let back = h.back(&again).unwrap();
    let mut g2 = back.clone();
    g2.add(Kind::Eq, 0.0, 0.0);
    h.track(&g2, true);
    assert!(h.redo.is_empty());
    // nothing to undo at the start
    let mut h2 = History::default();
    assert!(h2.back(&g).is_none() && h2.forward(&g).is_none());
    // the number of steps is limited
    let mut h3 = History::default();
    let mut gg = Graph::default();
    h3.track(&gg, true);
    for _ in 0..History::MAX + 30 {
        gg.add(Kind::Gain, 0.0, 0.0);
        h3.track(&gg, true);
    }
    assert_eq!(h3.undo.len(), History::MAX);
}

#[test]
fn split_and_merge_ports_are_per_channel() {
    let mut g = Graph::default();
    let (i, s, m, a) = (g.add(Kind::Input, 0.0, 0.0), g.add(Kind::Split, 0.0, 0.0), g.add(Kind::Merge, 0.0, 0.0), g.add(Kind::Analyzer, 0.0, 0.0));
    g.connect(i, s, 0).unwrap();
    g.connect_from(s, 0, m, 0).unwrap();
    g.connect_from(s, 1, m, 1).unwrap();
    g.connect(m, a, 0).unwrap();
    assert!(g.connect_from(s, 2, m, 0).is_err(), "a split node has two outputs");
    assert!(g.connect_from(i, 1, s, 0).is_err(), "an input device has one");
    assert_eq!(g.edge_into(m, 1).unwrap().from_port, 1);
    let h = Graph::decode(&g.encode()).unwrap();
    assert_eq!(h.edge_into(m, 1).unwrap().from_port, 1);
    // the two outputs sit one row apart and the rows line up with the merge inputs
    let (sn, mn) = (g.node(s).unwrap(), g.node(m).unwrap());
    assert!((out_pos(sn, 1).y - out_pos(sn, 0).y - ROW).abs() < 1e-4);
    assert!((in_pos(mn, 1).y - in_pos(mn, 0).y - ROW).abs() < 1e-4);
}

#[test]
fn the_dats_set_up_adds_nodes_and_mutes_the_old_output() {
    let mut r = Routing::new();
    let i0 = r.g.add(Kind::Input, 0.0, 0.0);
    let a0 = r.g.add(Kind::Analyzer, 0.0, 0.0);
    let g0 = r.g.add(Kind::Generator, 0.0, 0.0);
    let o0 = r.g.add(Kind::Output, 0.0, 0.0);
    r.g.node_mut(o0).unwrap().s = "wasapi:dats".into();
    r.g.connect(i0, a0, 0).unwrap();
    r.g.connect(g0, o0, 0).unwrap();
    // the old output is a plain generator output: it gets muted and the nodes are made
    r.command("datsnodes	wasapi:datsin	wasapi:dats");
    r.apply_bound();
    let n = r.g.nodes.len();
    assert_eq!(n, 4 + 3, "{}", r.note);
    assert!(r.g.node(o0).unwrap().p[1] >= 0.5 || r.g.nodes.iter().filter(|x| x.kind == Kind::Output && x.s == "wasapi:dats").count() == 2);
    let new_out = r.g.nodes.iter().filter(|x| x.kind == Kind::Output && x.s == "wasapi:dats" && x.id != o0).next().unwrap();
    assert!(new_out.p[1] < 0.5);
    assert_eq!(r.g.node(g0).map(|x| x.kind), Some(Kind::Generator));
    assert!(r.g.nodes.iter().any(|x| x.kind == Kind::Input && x.s == "wasapi:datsin"));
    // the DATS input is wired to the analyzer (the old input node stays)
    let di = r.g.nodes.iter().find(|x| x.kind == Kind::Input && x.s == "wasapi:datsin").unwrap().id;
    assert_eq!(r.g.edge_into(a0, 0).unwrap().from, di);
    assert!(r.g.node(i0).is_some());
    // pressing it again changes nothing
    r.command("datsnodes	wasapi:datsin	wasapi:dats");
    r.apply_bound();
    assert_eq!(r.g.nodes.len(), n, "{}", r.note);
}

#[test]
fn a_device_request_sets_the_first_input_and_output_node() {
    let mut r = Routing::new();
    let i2 = r.g.add(Kind::Input, 0.0, 0.0);
    let _o = r.g.add(Kind::Output, 0.0, 0.0);
    let i1_before = r.g.nodes.iter().find(|n| n.kind == Kind::Input).unwrap().id;
    r.command("setdev	in	asio:DATS");
    r.command("setdev	out	wasapi:out1");
    r.apply_bound();
    let first_in = r.g.nodes.iter().filter(|n| n.kind == Kind::Input).map(|n| n.id).min().unwrap();
    assert_eq!(r.g.node(first_in).unwrap().s, "asio:DATS");
    assert_eq!(r.g.nodes.iter().find(|n| n.kind == Kind::Output).unwrap().s, "wasapi:out1");
    let _ = (i2, i1_before);
}

#[test]
fn port_positions_line_up_with_mixer_rows() {
    let mut g = Graph::default();
    let m = g.add(Kind::Mixer, 10.0, 20.0);
    let n = g.node(m).unwrap();
    // consecutive input dots are one row apart, the output sits in the header
    assert!((in_pos(n, 1).y - in_pos(n, 0).y - ROW).abs() < 1e-4);
    assert!(out_pos(n, 0).y < in_pos(n, 0).y);
    assert!(out_pos(n, 0).x > in_pos(n, 0).x);
}
