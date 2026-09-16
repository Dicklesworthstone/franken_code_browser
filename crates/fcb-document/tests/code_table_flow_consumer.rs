#![forbid(unsafe_code)]

//! Consumer integration tests verifying FCB document display plans consume
//! upstream FrankenMarkdown code fence flow and constrained table layout (FCB-033.A).
//!
//! Confirms:
//! 1. `CodeFenceFlow` display lists and exact code copying integrate cleanly into `DocumentDisplayPlan`.
//! 2. Line windowing avoids whole-fence clones on giant code blocks.
//! 3. `ConstrainedTableFlow` with row virtualization and horizontal scrolling adapts into the consumer plan.
//! 4. Accessible reading order correctly exposes `CodeBlock` and `Table` semantic nodes.
//! 5. Document generation validation protects display plans from stale requests.
//! 6. Negative controls: Stale generation delivery and table column budget overflows are refused.

use fcb_core::{ArenaOwnerId, DocumentGeneration};
use fcb_document::DocumentDisplayPlan;
use franken_markdown::ast::Align;
use franken_markdown::code_table_flow::{
    CodeFenceFlow, CodeTableError, ConstrainedTableFlow, TableCell, TableConstraints,
    MAX_COLUMNS_BUDGET,
};
use franken_markdown::display::{AccessibleReadingRole, DisplayItem, DisplayRect};
use franken_markdown::span::SourceSpan;

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(42).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

#[test]
fn code_fence_and_exact_copy_integrate_into_document_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let code_src = "pub fn execute() -> Result<(), Error> {\n    let mut state = State::init();\n    state.step()\n}\n";
    let span = SourceSpan::new(50, 50 + code_src.len());
    let code_flow = CodeFenceFlow::new(Some("rust".to_string()), code_src.to_string(), span);

    // Exact copy invariant
    assert_eq!(code_flow.exact_code_copy(), code_src);

    let bounds = DisplayRect::new(0.0, 0.0, 700.0, code_flow.total_height());
    let display_list = code_flow
        .materialize_viewport(bounds, 0.0, 400.0)
        .expect("materialize code fence");

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());

    assert_eq!(plan.generation(), doc_gen);
    assert!(plan.is_complete());
    assert!(plan.item_count() > 0);

    // Verify accessibility node
    let reading_nodes = plan.reading_order();
    assert_eq!(reading_nodes.len(), 1);
    assert_eq!(reading_nodes[0].role, AccessibleReadingRole::CodeBlock);
    assert_eq!(reading_nodes[0].text, code_src);

    // Delivery validation
    assert!(plan.validate_delivery(doc_gen).is_ok());
    let stale_gen = test_generation(owner, 99);
    assert!(plan.validate_delivery(stale_gen).is_err());
}

#[test]
fn constrained_table_and_virtualization_integrate_into_document_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    let headers = vec![
        TableCell::new("Service", SourceSpan::default()),
        TableCell::new("Endpoint", SourceSpan::default()),
        TableCell::new("Method", SourceSpan::default()),
        TableCell::new("AuthRequired", SourceSpan::default()),
    ];
    let alignments = vec![Align::Left; 4];

    // 500 rows to test virtualization
    let mut rows = Vec::new();
    for i in 0..500 {
        rows.push(vec![
            TableCell::new(format!("Service_{i}"), SourceSpan::default()),
            TableCell::new(format!("/api/v1/resource/{i}"), SourceSpan::default()),
            TableCell::new("POST", SourceSpan::default()),
            TableCell::new("true", SourceSpan::default()),
        ]);
    }

    let constraints = TableConstraints {
        min_col_width: 80.0,
        max_col_width: 250.0,
        container_width: 600.0,
        pinned_headers: true,
    };

    let table = ConstrainedTableFlow::try_new(
        alignments,
        headers,
        rows,
        SourceSpan::default(),
        constraints,
    )
    .expect("build constrained table");

    // Materialize viewport: 300pt height at offset 200pt
    let display_list = table
        .materialize_viewport(0.0, 0.0, 200.0, 300.0)
        .expect("materialize table viewport");

    let plan = DocumentDisplayPlan::new(doc_gen, display_list, Vec::new());
    assert!(plan.is_complete());

    // Row virtualization: only a fraction of cells are emitted
    let cell_count = plan
        .items()
        .iter()
        .filter(|i| match i {
            DisplayItem::Text(t) => t.color_role == "table-cell",
            _ => false,
        })
        .count();

    assert!(cell_count > 0);
    assert!(
        cell_count <= 60,
        "virtualized table must only emit visible row cells, got {cell_count}"
    );

    // Reading order contains the Table accessible node with header row child
    let reading_nodes = plan.reading_order();
    assert_eq!(reading_nodes.len(), 1);
    assert_eq!(reading_nodes[0].role, AccessibleReadingRole::Table);
    assert!(!reading_nodes[0].children.is_empty());
    assert_eq!(
        reading_nodes[0].children[0].role,
        AccessibleReadingRole::TableHeaderRow
    );
}

#[test]
fn consumer_pinned_semantic_headers_and_row_virtualization_in_display_plan() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 2);

    let headers = vec![
        TableCell::new("Metric", SourceSpan::default()),
        TableCell::new("Value", SourceSpan::default()),
    ];
    let alignments = vec![Align::Left, Align::Left];

    // 1,000 rows
    let mut rows = Vec::new();
    for i in 0..1000 {
        rows.push(vec![
            TableCell::new(format!("Metric_{i:04}"), SourceSpan::default()),
            TableCell::new(format!("{}", i * 10), SourceSpan::default()),
        ]);
    }

    let table = ConstrainedTableFlow::try_new(
        alignments,
        headers,
        rows,
        SourceSpan::default(),
        TableConstraints::default(),
    )
    .unwrap();

    // Deep scroll offset at 3,000pt
    let dl = table
        .materialize_viewport(0.0, 0.0, 3000.0, 400.0)
        .expect("materialize scrolled table");

    let plan = DocumentDisplayPlan::new(doc_gen, dl, Vec::new());
    assert!(plan.is_complete());

    // Header text run must pin to 3000.0pt
    let header_runs: Vec<_> = plan
        .items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Text(t) if t.color_role == "table-header" => Some(t),
            _ => None,
        })
        .collect();

    assert_eq!(header_runs.len(), 2);
    for h in header_runs {
        assert_eq!(h.bounds.y, 3000.0, "header run must pin to viewport top");
    }

    // Accessible reading tree contains TableHeaderRow and visible TableRow children
    let table_node = &plan.reading_order()[0];
    assert_eq!(table_node.role, AccessibleReadingRole::Table);
    assert_eq!(table_node.children[0].role, AccessibleReadingRole::TableHeaderRow);
    assert_eq!(
        table_node.children[0].children[0].role,
        AccessibleReadingRole::TableHeaderCell
    );

    // Visible body row children follow the header
    assert!(table_node.children.len() > 1);
    assert_eq!(table_node.children[1].role, AccessibleReadingRole::TableRow);
    assert_eq!(
        table_node.children[1].children[0].role,
        AccessibleReadingRole::TableCell
    );
}

#[test]
fn consumer_stable_estimates_and_batch_refinement() {
    let headers = vec![
        TableCell::new("Key", SourceSpan::default()),
        TableCell::new("Val", SourceSpan::default()),
    ];
    let alignments = vec![Align::Left, Align::Left];

    // 400 rows
    let mut rows = Vec::new();
    for i in 0..400 {
        let val = if i == 250 {
            "Wide payload in row 250".to_string()
        } else {
            format!("v_{i:04}")
        };
        rows.push(vec![
            TableCell::new(format!("k_{i:04}"), SourceSpan::default()),
            TableCell::new(val, SourceSpan::default()),
        ]);
    }

    let mut table = ConstrainedTableFlow::try_new(
        alignments,
        headers,
        rows,
        SourceSpan::default(),
        TableConstraints::default(),
    )
    .unwrap();

    // Starts in Provisional state
    assert!(!table.is_fully_measured());
    assert_eq!(table.measured_rows_count(), 100);

    let initial_w = table.column_width(1).unwrap();

    // Incremental batch measurement
    let ref1 = table.measure_batch(100).unwrap();
    assert_eq!(ref1.measured_rows, 200);
    assert!(!ref1.reflow_required);

    // Batch containing row 250 triggers reflow_required
    let ref2 = table.measure_batch(100).unwrap();
    assert_eq!(ref2.measured_rows, 300);
    assert!(ref2.reflow_required);
    assert!(table.column_width(1).unwrap() > initial_w);

    // Completion
    let ref3 = table.complete_measurement().unwrap();
    assert!(ref3.is_complete);
    assert!(table.is_fully_measured());
}

#[test]
fn consumer_read_only_task_list_safety_contract() {
    use franken_markdown::block_flow::{BlockFlowEngine, FlowBlockItem, ListMarker, LogicalHeight};

    let items = vec![
        FlowBlockItem::ListItem {
            depth: 0,
            marker: ListMarker::Task { checked: true },
            text: "Task A done".to_string(),
            source_span: SourceSpan::new(0, 11),
        },
        FlowBlockItem::ListItem {
            depth: 0,
            marker: ListMarker::Task { checked: false },
            text: "Task B todo".to_string(),
            source_span: SourceSpan::new(12, 23),
        },
    ];

    let mut engine = BlockFlowEngine::new();
    for item in items {
        engine.push_block(item, 800.0).expect("push");
    }
    let dl = engine
        .materialize_viewport(0.0, LogicalHeight::from_points(0.0), 800.0, 400.0)
        .expect("materialize");

    let owner = test_owner();
    let doc_gen = test_generation(owner, 5);
    let plan = DocumentDisplayPlan::new(doc_gen, dl, Vec::new());

    // Invariant: no anchors for task items
    assert_eq!(plan.display_list().anchors().count(), 0);

    // Checkbox click hit-test returns non-anchor item
    let hit = plan.display_list().hit_test(0.0, 5.0);
    assert!(hit.is_some());
    assert!(!matches!(hit.unwrap(), DisplayItem::Anchor(_)));
}

#[test]
fn negative_controls_table_budget_and_stale_delivery() {
    let owner = test_owner();
    let doc_gen = test_generation(owner, 1);

    // Negative control: column budget overflow (>64)
    let too_many_cols = MAX_COLUMNS_BUDGET + 1;
    let headers = vec![TableCell::new("Header", SourceSpan::default()); too_many_cols];
    let alignments = vec![Align::Left; too_many_cols];

    let err = ConstrainedTableFlow::try_new(
        alignments,
        headers,
        Vec::new(),
        SourceSpan::default(),
        TableConstraints::default(),
    );

    assert!(matches!(
        err,
        Err(CodeTableError::ColumnBudgetExceeded { .. })
    ));

    // Negative control: stale delivery on valid plan
    let plan = DocumentDisplayPlan::new(doc_gen, franken_markdown::display::DisplayList::new(), Vec::new());
    let other_owner = ArenaOwnerId::new(999).unwrap();
    let mismatched_gen = test_generation(other_owner, 1);
    assert!(plan.validate_delivery(mismatched_gen).is_err());
}
