//! Output adapters — Phase 6.
//!
//! Writes enriched records produced by the preprocessing pipeline to secondary
//! MongoDB collections in the destination database.  Two adapters are provided:
//!
//! * [`graph`]  — [`graph::GraphWriter`] persists [`RelationEdge`] records to
//!   `entity_edges` and [`ProvTriple`] records to `prov_relations`, enabling
//!   provenance-graph traversal queries.
//!
//! * [`vector`] — [`vector::VectorWriter`] persists content and behavioral
//!   [`EmbeddingRecord`] documents to `content_embeddings` and
//!   `behavioral_embeddings` respectively, enabling approximate
//!   nearest-neighbour similarity search.
//!
//! Both writers are **idempotent**: re-processing the same sample replaces
//! existing documents rather than creating duplicates.  Index management is
//! handled lazily on first write.
//!
//! # Usage
//! ```rust,ignore
//! let graph_writer  = GraphWriter::new(repo.destination_db());
//! let vector_writer = VectorWriter::new(repo.destination_db());
//!
//! graph_writer.write_edges(&metadata.relations).await?;
//! graph_writer.write_prov(&prov_triples).await?;
//!
//! let records = embedding_result.into_records(&config.embedding.model);
//! vector_writer.write(&records).await?;
//! ```
//!
//! [`RelationEdge`]:  crate::models::RelationEdge
//! [`ProvTriple`]:    crate::preprocessing::prov_linker::ProvTriple
//! [`EmbeddingRecord`]: crate::embedding::EmbeddingRecord

pub mod graph;
pub mod vector;
