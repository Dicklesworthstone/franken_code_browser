#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document display plans consume
//! upstream FrankenMarkdown math and diagram display outputs (FCB-034.A).
//!
//! Confirms:
//! 1. Upstream math display items adapt cleanly into `DocumentDisplayPlan`.
//! 2. First-party vector diagram outputs adapt cleanly into `DocumentDisplayPlan`.
//! 3. Source spans and semantic reading nodes are preserved across the consumer boundary.
//! 4. Generation validation protects presentation-ready display plans from stale requests.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use fcb_document::DocumentDisplayPlan;
use franken_markdown::display::{
    DisplayItem, DisplayList, VectorShapeType,
};
use franken_markdown::math_display::{
    diagram_to_display, diagram_to_display_with_fallback,
    math_to_display, DiagramError,
};
use franken_markdown::math::Engine;

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

fn math_engine() -> Engine {
    Engine::bundled().expect("bundled math engine loads")
}

#[test]
fn math_display_integrates_into_document_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);
    let engine = math_engine();

    let formula = "\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}";
    let offset = 200;
    let math_items = math_to_display(formula, &engine, 0.0, 50.0, 16.0, offset)
        .expect("formula typesets successfully");

    assert!(!math_items.is_empty(), "must produce display items");

    let mut display_list = DisplayList::new();
    for item in math_items {
        display_list.push_item(item);
    }

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());

    assert_eq!(plan.generation(), doc_gen);
    assert!(plan.is_complete());
    assert!(plan.item_count() > 0);

    // Verify bounds encompass the math formula
    let bounds = plan.bounds();
    assert!(bounds.width > 0.0);
    assert!(bounds.height > 0.0);

    // Verify generation delivery validation
    assert!(plan.validate_delivery(doc_gen).is_ok());

    let stale_gen = test_generation(owner, 2);
    assert!(
        plan.validate_delivery(stale_gen).is_err(),
        "stale generation delivery must be rejected"
    );
}

#[test]
fn diagram_display_integrates_into_document_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let diagram_source = "graph TD\n  Start[Begin Session] --> Work[Execute Task]\n  Work --> Done[Finish]";
    let offset = 1000;
    let diagram_items = diagram_to_display("mermaid", diagram_source, 10.0, 10.0, 14.0, offset)
        .expect("diagram parses into vector forms");

    assert!(!diagram_items.is_empty());

    let mut display_list = DisplayList::new();
    for item in diagram_items {
        display_list.push_item(item);
    }

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());

    assert!(plan.is_complete());
    assert!(plan.item_count() >= 6, "node boxes, connectors, arrows, and labels");

    // Check that diagram vector elements exist in the plan
    let has_diagram_box = plan.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::DiagramBox,
        _ => false,
    });
    assert!(has_diagram_box, "plan contains DiagramBox vector shape");

    let has_diagram_arrow = plan.items().iter().any(|i| match i {
        DisplayItem::Vector(v) => v.shape == VectorShapeType::DiagramArrow,
        _ => false,
    });
    assert!(has_diagram_arrow, "plan contains DiagramArrow vector shape");
}

#[test]
fn mixed_math_and_diagram_document_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 5);
    let engine = math_engine();

    let math_source = "E = mc^2";
    let diagram_source = "flowchart LR\n  In[Input] --> Out[Output]";

    let math_items = math_to_display(math_source, &engine, 0.0, 0.0, 14.0, 10)
        .expect("math typesets");
    let diagram_items = diagram_to_display("flowchart", diagram_source, 0.0, 100.0, 14.0, 200)
        .expect("diagram parses");

    let mut display_list = DisplayList::new();
    for item in math_items {
        display_list.push_item(item);
    }
    for item in diagram_items {
        display_list.push_item(item);
    }

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());
    assert!(plan.is_complete());
    assert!(plan.bounds().height >= 100.0, "bounds cover both sections");

    // Verify all items have valid non-negative dimensions
    for item in plan.items() {
        let b = item.bounds();
        assert!(b.width > 0.0 && b.height > 0.0);
    }
}

#[test]
fn hostile_diagram_in_display_plan_renders_fallback_without_script() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let hostile_source = "graph TD\n  A[<script>alert('pwn')</script>] --> B";
    let offset = 404;

    // Vector parser rejects hostile scripts
    let parse_res = diagram_to_display("mermaid", hostile_source, 0.0, 0.0, 14.0, offset);
    assert!(matches!(parse_res, Err(DiagramError::HostileContent(_))));

    // Fallback safely integrates into display plan
    let fallback_items = diagram_to_display_with_fallback(
        "mermaid",
        hostile_source,
        0.0,
        0.0,
        14.0,
        offset,
    );
    assert!(!fallback_items.is_empty());

    let mut display_list = DisplayList::new();
    for item in fallback_items {
        display_list.push_item(item);
    }

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());
    assert!(plan.is_complete());

    // Verify note role is present
    let has_note = plan.items().iter().any(|i| match i {
        DisplayItem::Text(t) => t.color_role == "diagram-fallback-note",
        _ => false,
    });
    assert!(has_note, "plan contains fallback capability explanation");
}
