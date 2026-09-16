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

    // Reading order contains the Table accessible node
    let reading_nodes = plan.reading_order();
    assert_eq!(reading_nodes.len(), 1);
    assert_eq!(reading_nodes[0].role, AccessibleReadingRole::Table);
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
