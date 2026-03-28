//! io_uring High-Performance Async I/O
//!
//! This module provides io_uring integration for ultra-low latency network I/O
//! operations on Linux. io_uring can provide 3-5x throughput improvements over
//! traditional async I/O for network-intensive workloads.

#[cfg(feature = "performance-opt")]
use io_uring::{IoUring, opcode, types};

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{Result, Context};
use bytes::{Bytes, BytesMut};
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

/// io_uring manager for high-performance network operations
#[cfg(feature = "performance-opt")]
pub struct IoUringManager {
    ring: Arc<Mutex<IoUring>>,
    queue_depth: u32,
    stats: Arc<IoUringStats>,
    completion_handler: Option<tokio::task::JoinHandle<()>>,
    operation_queue: Arc<Mutex<VecDeque<PendingOperation>>>,
}

#[cfg(feature = "performance-opt")]
impl IoUringManager {
    /// Create new io_uring manager
    pub fn new(queue_depth: u32) -> Result<Self> {
        let ring = IoUring::new(queue_depth)
            .context("Failed to create io_uring instance")?;

        let stats = Arc::new(IoUringStats::default());
        let operation_queue = Arc::new(Mutex::new(VecDeque::new()));

        let manager = Self {
            ring: Arc::new(Mutex::new(ring)),
            queue_depth,
            stats: stats.clone(),
            completion_handler: None,
            operation_queue,
        };

        tracing::info!("Created io_uring manager with queue depth {}", queue_depth);
        Ok(manager)
    }

    /// Start the completion handler task
    pub fn start(&mut self) -> Result<()> {
        let ring = self.ring.clone();
        let stats = self.stats.clone();
        let operation_queue = self.operation_queue.clone();

        let handler = tokio::task::spawn_blocking(move || {
            Self::completion_loop(ring, stats, operation_queue)
        });

        self.completion_handler = Some(handler);
        tracing::info!("Started io_uring completion handler");
        Ok(())
    }

    /// Submit batch of UDP send operations
    pub async fn send_batch(
        &self,
        socket: std::os::unix::io::RawFd,
        operations: Vec<UdpSendOp>,
    ) -> Result<Vec<Result<usize>>> {
        let (tx, rx) = oneshot::channel();
        let batch_id = self.stats.operations_submitted.fetch_add(1, Ordering::Relaxed);

        // Queue batch operation
        {
            let mut queue = self.operation_queue.lock();
            queue.push_back(PendingOperation {
                id: batch_id,
                op_type: OperationType::BatchSend(operations),
                socket,
                completion_tx: tx,
                submitted_at: Instant::now(),
            });
        }

        // Submit to io_uring
        self.submit_queued_operations()?;

        // Wait for completion
        let results = rx.await
            .context("io_uring operation channel closed")?;

        if let OperationResult::BatchSend(send_results) = results {
            Ok(send_results)
        } else {
            anyhow::bail!("Unexpected operation result type")
        }
    }

    /// Submit batch of UDP receive operations
    pub async fn recv_batch(
        &self,
        socket: std::os::unix::io::RawFd,
        buffer_size: usize,
        count: usize,
    ) -> Result<Vec<Result<(Bytes, SocketAddr)>>> {
        let (tx, rx) = oneshot::channel();
        let batch_id = self.stats.operations_submitted.fetch_add(1, Ordering::Relaxed);

        {
            let mut queue = self.operation_queue.lock();
            queue.push_back(PendingOperation {
                id: batch_id,
                op_type: OperationType::BatchRecv { buffer_size, count },
                socket,
                completion_tx: tx,
                submitted_at: Instant::now(),
            });
        }

        self.submit_queued_operations()?;

        let results = rx.await
            .context("io_uring operation channel closed")?;

        if let OperationResult::BatchRecv(recv_results) = results {
            Ok(recv_results)
        } else {
            anyhow::bail!("Unexpected operation result type")
        }
    }

    /// Submit single optimized send operation
    pub async fn send_optimized(
        &self,
        socket: std::os::unix::io::RawFd,
        data: Bytes,
        addr: SocketAddr,
    ) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        let op_id = self.stats.operations_submitted.fetch_add(1, Ordering::Relaxed);

        {
            let mut queue = self.operation_queue.lock();
            queue.push_back(PendingOperation {
                id: op_id,
                op_type: OperationType::SingleSend { data, addr },
                socket,
                completion_tx: tx,
                submitted_at: Instant::now(),
            });
        }

        self.submit_queued_operations()?;

        let result = rx.await
            .context("io_uring operation channel closed")?;

        if let OperationResult::SingleSend(send_result) = result {
            send_result
        } else {
            anyhow::bail!("Unexpected operation result type")
        }
    }

    /// Submit queued operations to io_uring
    fn submit_queued_operations(&self) -> Result<()> {
        let mut ring = self.ring.lock();
        let mut queue = self.operation_queue.lock();

        let available_slots = self.queue_depth as usize - ring.submission().len();
        let to_submit = queue.len().min(available_slots);

        for _ in 0..to_submit {
            if let Some(pending_op) = queue.pop_front() {
                match pending_op.op_type {
                    OperationType::SingleSend { ref data, addr } => {
                        self.submit_send_op(&mut ring, &pending_op, data, addr)?;
                    }
                    OperationType::BatchSend(ref operations) => {
                        self.submit_batch_send_ops(&mut ring, &pending_op, operations)?;
                    }
                    OperationType::BatchRecv { buffer_size, count } => {
                        self.submit_batch_recv_ops(&mut ring, &pending_op, buffer_size, count)?;
                    }
                }
            }
        }

        // Submit all queued operations
        ring.submit()
            .context("Failed to submit io_uring operations")?;

        self.stats.operations_submitted.fetch_add(to_submit as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Submit single send operation
    fn submit_send_op(
        &self,
        ring: &mut IoUring,
        pending_op: &PendingOperation,
        data: &Bytes,
        addr: SocketAddr,
    ) -> Result<()> {
        let send_entry = opcode::SendTo::new(
            types::Fd(pending_op.socket),
            data.as_ptr(),
            data.len() as u32,
        )
        .dest_addr(&addr as *const SocketAddr as *const libc::sockaddr)
        .build()
        .user_data(pending_op.id);

        unsafe {
            ring.submission()
                .push(&send_entry)
                .context("Failed to push send operation to io_uring")?;
        }

        Ok(())
    }

    /// Submit batch send operations
    fn submit_batch_send_ops(
        &self,
        ring: &mut IoUring,
        pending_op: &PendingOperation,
        operations: &[UdpSendOp],
    ) -> Result<()> {
        for (i, op) in operations.iter().enumerate() {
            let send_entry = opcode::SendTo::new(
                types::Fd(pending_op.socket),
                op.data.as_ptr(),
                op.data.len() as u32,
            )
            .dest_addr(&op.addr as *const SocketAddr as *const libc::sockaddr)
            .build()
            .user_data(pending_op.id * 1000 + i as u64); // Unique ID per batch item

            unsafe {
                ring.submission()
                    .push(&send_entry)
                    .context("Failed to push batch send operation to io_uring")?;
            }
        }

        Ok(())
    }

    /// Submit batch receive operations
    fn submit_batch_recv_ops(
        &self,
        ring: &mut IoUring,
        pending_op: &PendingOperation,
        buffer_size: usize,
        count: usize,
    ) -> Result<()> {
        for i in 0..count {
            let mut buffer = BytesMut::with_capacity(buffer_size);
            buffer.resize(buffer_size, 0);

            let recv_entry = opcode::RecvFrom::new(
                types::Fd(pending_op.socket),
                buffer.as_mut_ptr(),
                buffer_size as u32,
            )
            .build()
            .user_data(pending_op.id * 1000 + i as u64);

            unsafe {
                ring.submission()
                    .push(&recv_entry)
                    .context("Failed to push batch recv operation to io_uring")?;
            }
        }

        Ok(())
    }

    /// Main completion loop (runs in blocking task)
    fn completion_loop(
        ring: Arc<Mutex<IoUring>>,
        stats: Arc<IoUringStats>,
        operation_queue: Arc<Mutex<VecDeque<PendingOperation>>>,
    ) {
        loop {
            let completions = {
                let mut ring = ring.lock();
                let mut cq = ring.completion();
                let mut completions = Vec::new();

                while let Some(cqe) = cq.next() {
                    completions.push((cqe.user_data(), cqe.result()));
                }

                completions
            };

            for (user_data, result) in completions {
                stats.operations_completed.fetch_add(1, Ordering::Relaxed);

                if result < 0 {
                    stats.operations_failed.fetch_add(1, Ordering::Relaxed);
                } else {
                    stats.bytes_transferred.fetch_add(result as u64, Ordering::Relaxed);
                }

                // Handle completion based on operation type
                // This is simplified - real implementation would track operations
                // and complete the appropriate futures
            }

            // Brief sleep to prevent busy-waiting
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    /// Get performance statistics
    pub fn get_stats(&self) -> IoUringStats {
        (*self.stats).clone()
    }

    /// Shutdown the io_uring manager
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(handler) = self.completion_handler.take() {
            handler.abort();
            let _ = handler.await;
        }

        tracing::info!("Shutdown io_uring manager");
        Ok(())
    }
}

/// Pending io_uring operation
#[cfg(feature = "performance-opt")]
struct PendingOperation {
    id: u64,
    op_type: OperationType,
    socket: std::os::unix::io::RawFd,
    completion_tx: oneshot::Sender<OperationResult>,
    submitted_at: Instant,
}

/// Operation types for io_uring
#[cfg(feature = "performance-opt")]
enum OperationType {
    SingleSend { data: Bytes, addr: SocketAddr },
    BatchSend(Vec<UdpSendOp>),
    BatchRecv { buffer_size: usize, count: usize },
}

/// Operation results
#[cfg(feature = "performance-opt")]
enum OperationResult {
    SingleSend(Result<usize>),
    BatchSend(Vec<Result<usize>>),
    BatchRecv(Vec<Result<(Bytes, SocketAddr)>>),
}

/// UDP send operation
#[derive(Clone)]
pub struct UdpSendOp {
    pub data: Bytes,
    pub addr: SocketAddr,
}

/// io_uring performance statistics
#[derive(Debug, Clone, Default)]
pub struct IoUringStats {
    /// Total operations submitted to io_uring
    pub operations_submitted: AtomicU64,
    /// Total operations completed
    pub operations_completed: AtomicU64,
    /// Total failed operations
    pub operations_failed: AtomicU64,
    /// Total bytes transferred
    pub bytes_transferred: AtomicU64,
    /// Average completion latency (microseconds)
    pub avg_completion_latency_us: AtomicU64,
}

impl IoUringStats {
    /// Calculate operations per second
    pub fn ops_per_second(&self, duration: Duration) -> f64 {
        let ops = self.operations_completed.load(Ordering::Relaxed) as f64;
        let seconds = duration.as_secs_f64();
        if seconds > 0.0 { ops / seconds } else { 0.0 }
    }

    /// Calculate throughput in bytes per second
    pub fn throughput_bps(&self, duration: Duration) -> f64 {
        let bytes = self.bytes_transferred.load(Ordering::Relaxed) as f64;
        let seconds = duration.as_secs_f64();
        if seconds > 0.0 { bytes / seconds } else { 0.0 }
    }

    /// Calculate error rate
    pub fn error_rate(&self) -> f64 {
        let completed = self.operations_completed.load(Ordering::Relaxed) as f64;
        let failed = self.operations_failed.load(Ordering::Relaxed) as f64;
        let total = completed + failed;
        if total > 0.0 { failed / total } else { 0.0 }
    }
}

/// Fallback implementation for non-Linux systems
#[cfg(not(feature = "performance-opt"))]
pub struct IoUringManager;

#[cfg(not(feature = "performance-opt"))]
impl IoUringManager {
    pub fn new(_queue_depth: u32) -> Result<Self> {
        anyhow::bail!("io_uring not available on this platform")
    }

    pub fn get_stats(&self) -> IoUringStats {
        IoUringStats::default()
    }
}

/// Enhanced UDP socket with io_uring optimization
pub struct IoUringSocket {
    #[cfg(feature = "performance-opt")]
    inner: tokio::net::UdpSocket,

    #[cfg(feature = "performance-opt")]
    io_uring: Option<Arc<IoUringManager>>,

    #[cfg(not(feature = "performance-opt"))]
    inner: tokio::net::UdpSocket,

    enable_batching: bool,
    batch_buffer: Arc<Mutex<Vec<UdpSendOp>>>,
}

impl IoUringSocket {
    /// Create new io_uring-enhanced UDP socket
    pub fn new(
        socket: tokio::net::UdpSocket,
        #[cfg(feature = "performance-opt")]
        io_uring: Option<Arc<IoUringManager>>,
    ) -> Self {
        Self {
            inner: socket,
            #[cfg(feature = "performance-opt")]
            io_uring,
            enable_batching: true,
            batch_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Send data with io_uring optimization if available
    pub async fn send_to(&self, data: &[u8], addr: SocketAddr) -> Result<usize> {
        #[cfg(feature = "performance-opt")]
        if let Some(ref io_uring) = self.io_uring {
            // Use io_uring for optimized sending
            let fd = self.inner.as_raw_fd();
            return io_uring.send_optimized(fd, Bytes::copy_from_slice(data), addr).await;
        }

        // Fallback to standard tokio
        self.inner.send_to(data, addr).await
            .context("Failed to send UDP packet")
    }

    /// Receive data with io_uring optimization if available
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        // For single receives, tokio is often sufficient
        // io_uring shines with batched operations
        self.inner.recv_from(buf).await
            .context("Failed to receive UDP packet")
    }

    /// Add operation to batch buffer
    pub fn queue_send(&self, data: Bytes, addr: SocketAddr) {
        if self.enable_batching {
            let mut buffer = self.batch_buffer.lock();
            buffer.push(UdpSendOp { data, addr });
        }
    }

    /// Flush batch buffer using io_uring
    pub async fn flush_batch(&self) -> Result<Vec<Result<usize>>> {
        let operations = {
            let mut buffer = self.batch_buffer.lock();
            let ops = buffer.clone();
            buffer.clear();
            ops
        };

        if operations.is_empty() {
            return Ok(Vec::new());
        }

        #[cfg(feature = "performance-opt")]
        if let Some(ref io_uring) = self.io_uring {
            let fd = self.inner.as_raw_fd();
            return io_uring.send_batch(fd, operations).await;
        }

        // Fallback: send operations individually
        let mut results = Vec::with_capacity(operations.len());
        for op in operations {
            let result = self.inner.send_to(&op.data, op.addr).await
                .map_err(Into::into);
            results.push(result);
        }
        Ok(results)
    }
}

#[cfg(feature = "performance-opt")]
use std::os::unix::io::AsRawFd;

#[cfg(not(feature = "performance-opt"))]
trait AsRawFd {
    fn as_raw_fd(&self) -> i32 { 0 }
}

#[cfg(not(feature = "performance-opt"))]
impl AsRawFd for tokio::net::UdpSocket {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_uring_stats_calculations() {
        let stats = IoUringStats::default();

        // Test with zero values
        let duration = Duration::from_secs(1);
        assert_eq!(stats.ops_per_second(duration), 0.0);
        assert_eq!(stats.throughput_bps(duration), 0.0);
        assert_eq!(stats.error_rate(), 0.0);
    }

    #[test]
    fn test_udp_send_op_clone() {
        let op = UdpSendOp {
            data: Bytes::from_static(b"test"),
            addr: "127.0.0.1:8080".parse().unwrap(),
        };

        let cloned = op.clone();
        assert_eq!(op.data, cloned.data);
        assert_eq!(op.addr, cloned.addr);
    }

    #[cfg(not(feature = "performance-opt"))]
    #[test]
    fn test_io_uring_manager_fallback() {
        let result = IoUringManager::new(256);
        assert!(result.is_err());
    }

    #[test]
    fn test_io_uring_stats_error_rate() {
        let stats = IoUringStats::default();
        stats.operations_completed.store(80, Ordering::Relaxed);
        stats.operations_failed.store(20, Ordering::Relaxed);

        let error_rate = stats.error_rate();
        assert!((error_rate - 0.2).abs() < f64::EPSILON);
    }
}