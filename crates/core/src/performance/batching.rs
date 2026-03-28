//! High-Performance Network Operation Batching
//!
//! This module provides batching capabilities for network operations to reduce
//! system call overhead and improve throughput. Features include:
//! - Adaptive batching based on load conditions
//! - Batch coalescing with configurable timeouts
//! - Priority-based operation queuing
//! - Batch size optimization based on network conditions

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::collections::{VecDeque, BinaryHeap};
use std::cmp::Reverse;
use std::time::{Duration, Instant};
use std::net::SocketAddr;

use anyhow::{Result, Context};
use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::sleep;

/// High-performance batch manager
pub struct BatchManager {
    sender_queues: RwLock<Vec<Arc<SenderQueue>>>,
    receiver_queues: RwLock<Vec<Arc<ReceiverQueue>>>,
    config: BatchConfig,
    stats: Arc<BatchStats>,
    flush_notifier: Arc<Notify>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl BatchManager {
    /// Create new batch manager
    pub fn new(batch_size: usize) -> Result<Self> {
        let config = BatchConfig {
            max_batch_size: batch_size,
            batch_timeout: Duration::from_millis(10), // 10ms timeout
            priority_levels: 3,
            adaptive_sizing: true,
            min_batch_size: 4,
            coalescing_window: Duration::from_micros(500), // 500μs coalescing
        };

        let stats = Arc::new(BatchStats::default());
        let flush_notifier = Arc::new(Notify::new());

        let mut manager = Self {
            sender_queues: RwLock::new(Vec::new()),
            receiver_queues: RwLock::new(Vec::new()),
            config,
            stats,
            flush_notifier,
            shutdown_tx: None,
        };

        manager.start_flush_timer()?;
        Ok(manager)
    }

    /// Create new sender queue for batched operations
    pub fn create_sender_queue(&self, priority: Priority) -> Arc<SenderQueue> {
        let queue = Arc::new(SenderQueue::new(
            self.config.clone(),
            self.stats.clone(),
            priority,
        ));

        {
            let mut queues = self.sender_queues.write();
            queues.push(queue.clone());
        }

        queue
    }

    /// Create new receiver queue for batched operations
    pub fn create_receiver_queue(&self) -> Arc<ReceiverQueue> {
        let queue = Arc::new(ReceiverQueue::new(
            self.config.clone(),
            self.stats.clone(),
        ));

        {
            let mut queues = self.receiver_queues.write();
            queues.push(queue.clone());
        }

        queue
    }

    /// Start periodic flush timer
    fn start_flush_timer(&mut self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(1);
        self.shutdown_tx = Some(tx);

        let sender_queues = self.sender_queues.clone();
        let receiver_queues = self.receiver_queues.clone();
        let timeout = self.config.batch_timeout;
        let notifier = self.flush_notifier.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(timeout / 2);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        // Periodic flush
                        Self::flush_all_queues(&sender_queues, &receiver_queues).await;
                    }
                    _ = notifier.notified() => {
                        // Immediate flush requested
                        Self::flush_all_queues(&sender_queues, &receiver_queues).await;
                    }
                    _ = rx.recv() => {
                        // Shutdown signal
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Flush all queues
    async fn flush_all_queues(
        sender_queues: &RwLock<Vec<Arc<SenderQueue>>>,
        receiver_queues: &RwLock<Vec<Arc<ReceiverQueue>>>,
    ) {
        // Flush sender queues by priority
        let sender_queues = sender_queues.read();
        let mut priority_queues: Vec<_> = sender_queues.iter().collect();
        priority_queues.sort_by_key(|q| q.priority);

        for queue in priority_queues {
            if queue.should_flush() {
                let _ = queue.flush().await;
            }
        }

        // Flush receiver queues
        let receiver_queues = receiver_queues.read();
        for queue in receiver_queues.iter() {
            if queue.should_flush() {
                let _ = queue.flush().await;
            }
        }
    }

    /// Request immediate flush of all queues
    pub fn flush_now(&self) {
        self.flush_notifier.notify_one();
    }

    /// Get batch statistics
    pub fn get_stats(&self) -> BatchStats {
        (*self.stats).clone()
    }

    /// Shutdown batch manager
    pub async fn shutdown(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Final flush of all queues
        Self::flush_all_queues(&self.sender_queues, &self.receiver_queues).await;

        Ok(())
    }
}

/// Batching configuration
#[derive(Clone, Debug)]
pub struct BatchConfig {
    /// Maximum batch size
    pub max_batch_size: usize,
    /// Batch timeout before forced flush
    pub batch_timeout: Duration,
    /// Number of priority levels
    pub priority_levels: u8,
    /// Enable adaptive batch sizing
    pub adaptive_sizing: bool,
    /// Minimum batch size before flush
    pub min_batch_size: usize,
    /// Time window for operation coalescing
    pub coalescing_window: Duration,
}

/// Operation priority levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Critical = 0,
    High = 1,
    Normal = 2,
    Low = 3,
}

/// Sender queue for batched send operations
pub struct SenderQueue {
    pending_operations: Mutex<PriorityQueue<SendOperation>>,
    config: BatchConfig,
    stats: Arc<BatchStats>,
    priority: Priority,
    last_flush: Mutex<Instant>,
    adaptive_batch_size: AtomicUsize,
}

impl SenderQueue {
    /// Create new sender queue
    pub fn new(config: BatchConfig, stats: Arc<BatchStats>, priority: Priority) -> Self {
        Self {
            pending_operations: Mutex::new(PriorityQueue::new()),
            config,
            stats,
            priority,
            last_flush: Mutex::new(Instant::now()),
            adaptive_batch_size: AtomicUsize::new(config.max_batch_size),
        }
    }

    /// Queue send operation for batching
    pub fn queue_send(
        &self,
        data: Bytes,
        addr: SocketAddr,
        priority: Priority,
    ) -> oneshot::Receiver<Result<usize>> {
        let (tx, rx) = oneshot::channel();

        let operation = SendOperation {
            data,
            addr,
            priority,
            completion_tx: tx,
            queued_at: Instant::now(),
        };

        {
            let mut queue = self.pending_operations.lock();
            queue.push(operation);
        }

        self.stats.operations_queued.fetch_add(1, Ordering::Relaxed);
        rx
    }

    /// Check if queue should be flushed
    pub fn should_flush(&self) -> bool {
        let queue = self.pending_operations.lock();
        let queue_size = queue.len();
        let last_flush = *self.last_flush.lock();

        // Flush if batch size reached
        if queue_size >= self.adaptive_batch_size.load(Ordering::Relaxed) {
            return true;
        }

        // Flush if timeout exceeded and have pending operations
        if !queue.is_empty() && last_flush.elapsed() >= self.config.batch_timeout {
            return true;
        }

        // Flush critical priority operations immediately
        if self.priority == Priority::Critical && !queue.is_empty() {
            return true;
        }

        false
    }

    /// Flush pending operations
    pub async fn flush(&self) -> Result<()> {
        let operations = {
            let mut queue = self.pending_operations.lock();
            if queue.is_empty() {
                return Ok(());
            }

            let mut ops = Vec::new();
            while let Some(op) = queue.pop() {
                ops.push(op);
            }
            ops
        };

        if operations.is_empty() {
            return Ok(());
        }

        let start = Instant::now();
        let batch_size = operations.len();

        // Execute batch send
        let results = self.execute_batch_send(operations).await?;

        // Update timing statistics
        let elapsed = start.elapsed();
        self.stats.batches_flushed.fetch_add(1, Ordering::Relaxed);
        self.stats.operations_executed.fetch_add(batch_size as u64, Ordering::Relaxed);
        self.stats.batch_execution_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        // Update adaptive batch size
        if self.config.adaptive_sizing {
            self.update_adaptive_batch_size(batch_size, elapsed);
        }

        // Update last flush time
        *self.last_flush.lock() = Instant::now();

        Ok(())
    }

    /// Execute batch send operations
    async fn execute_batch_send(&self, operations: Vec<SendOperation>) -> Result<()> {
        // Group operations by destination for efficiency
        use std::collections::HashMap;

        let mut addr_groups: HashMap<SocketAddr, Vec<SendOperation>> = HashMap::new();

        for op in operations {
            addr_groups.entry(op.addr).or_default().push(op);
        }

        // Execute each group
        for (addr, group) in addr_groups {
            // Simulate batch send (in real implementation, would use actual socket)
            let total_bytes: usize = group.iter().map(|op| op.data.len()).sum();

            // Complete all operations in the group
            for op in group {
                let _ = op.completion_tx.send(Ok(op.data.len()));

                // Track per-operation latency
                let latency = op.queued_at.elapsed();
                self.stats.total_latency_ns.fetch_add(latency.as_nanos() as u64, Ordering::Relaxed);
            }

            self.stats.bytes_sent.fetch_add(total_bytes as u64, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Update adaptive batch size based on performance
    fn update_adaptive_batch_size(&self, batch_size: usize, execution_time: Duration) {
        let current_size = self.adaptive_batch_size.load(Ordering::Relaxed);

        // Calculate throughput (operations per microsecond)
        let throughput = batch_size as f64 / execution_time.as_micros() as f64;

        // Adjust batch size based on throughput
        let new_size = if throughput > 1.0 && current_size < self.config.max_batch_size {
            // High throughput, increase batch size
            (current_size * 110 / 100).min(self.config.max_batch_size)
        } else if throughput < 0.5 && current_size > self.config.min_batch_size {
            // Low throughput, decrease batch size
            (current_size * 90 / 100).max(self.config.min_batch_size)
        } else {
            current_size
        };

        if new_size != current_size {
            self.adaptive_batch_size.store(new_size, Ordering::Relaxed);
            tracing::debug!(
                "Adjusted batch size from {} to {} (throughput: {:.2})",
                current_size, new_size, throughput
            );
        }
    }
}

/// Receiver queue for batched receive operations
pub struct ReceiverQueue {
    pending_buffers: Mutex<Vec<ReceiveBuffer>>,
    config: BatchConfig,
    stats: Arc<BatchStats>,
    last_flush: Mutex<Instant>,
}

impl ReceiverQueue {
    /// Create new receiver queue
    pub fn new(config: BatchConfig, stats: Arc<BatchStats>) -> Self {
        Self {
            pending_buffers: Mutex::new(Vec::new()),
            config,
            stats,
            last_flush: Mutex::new(Instant::now()),
        }
    }

    /// Queue receive buffer
    pub fn queue_receive(&self, buffer: ReceiveBuffer) {
        {
            let mut buffers = self.pending_buffers.lock();
            buffers.push(buffer);
        }

        self.stats.receive_buffers_queued.fetch_add(1, Ordering::Relaxed);
    }

    /// Check if queue should be flushed
    pub fn should_flush(&self) -> bool {
        let buffers = self.pending_buffers.lock();
        let buffer_count = buffers.len();
        let last_flush = *self.last_flush.lock();

        // Flush if buffer count reached
        if buffer_count >= self.config.max_batch_size {
            return true;
        }

        // Flush if timeout exceeded and have pending buffers
        if !buffers.is_empty() && last_flush.elapsed() >= self.config.batch_timeout {
            return true;
        }

        false
    }

    /// Flush pending receive buffers
    pub async fn flush(&self) -> Result<()> {
        let buffers = {
            let mut buffers = self.pending_buffers.lock();
            if buffers.is_empty() {
                return Ok(());
            }

            let mut result = Vec::new();
            result.extend(buffers.drain(..));
            result
        };

        if buffers.is_empty() {
            return Ok(());
        }

        let start = Instant::now();
        let buffer_count = buffers.len();

        // Execute batch receive
        self.execute_batch_receive(buffers).await?;

        // Update statistics
        let elapsed = start.elapsed();
        self.stats.receive_batches_flushed.fetch_add(1, Ordering::Relaxed);
        self.stats.receive_buffers_processed.fetch_add(buffer_count as u64, Ordering::Relaxed);
        self.stats.receive_execution_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        // Update last flush time
        *self.last_flush.lock() = Instant::now();

        Ok(())
    }

    /// Execute batch receive operations
    async fn execute_batch_receive(&self, buffers: Vec<ReceiveBuffer>) -> Result<()> {
        // Simulate batch receive processing
        let mut total_bytes = 0;

        for buffer in buffers {
            // Simulate processing received data
            total_bytes += buffer.capacity;

            // Complete the receive operation
            let _ = buffer.completion_tx.send(Ok((
                Bytes::from(vec![0u8; buffer.capacity]),
                "127.0.0.1:8080".parse().unwrap(),
            )));
        }

        self.stats.bytes_received.fetch_add(total_bytes as u64, Ordering::Relaxed);
        Ok(())
    }
}

/// Priority queue for send operations
struct PriorityQueue<T> {
    items: BinaryHeap<Reverse<PriorityItem<T>>>,
}

impl<T> PriorityQueue<T> {
    fn new() -> Self {
        Self {
            items: BinaryHeap::new(),
        }
    }

    fn push(&mut self, item: T)
    where
        T: HasPriority,
    {
        let priority_item = PriorityItem {
            priority: item.priority(),
            queued_at: Instant::now(),
            item,
        };
        self.items.push(Reverse(priority_item));
    }

    fn pop(&mut self) -> Option<T> {
        self.items.pop().map(|Reverse(item)| item.item)
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn len(&self) -> usize {
        self.items.len()
    }
}

/// Priority item wrapper
#[derive(Eq, PartialEq)]
struct PriorityItem<T> {
    priority: Priority,
    queued_at: Instant,
    item: T,
}

impl<T> Ord for PriorityItem<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Primary sort by priority (lower number = higher priority)
        match self.priority.cmp(&other.priority) {
            std::cmp::Ordering::Equal => {
                // Secondary sort by queue time (earlier = higher priority)
                other.queued_at.cmp(&self.queued_at)
            }
            other => other,
        }
    }
}

impl<T> PartialOrd for PriorityItem<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Trait for items with priority
trait HasPriority {
    fn priority(&self) -> Priority;
}

/// Send operation for batching
struct SendOperation {
    data: Bytes,
    addr: SocketAddr,
    priority: Priority,
    completion_tx: oneshot::Sender<Result<usize>>,
    queued_at: Instant,
}

impl HasPriority for SendOperation {
    fn priority(&self) -> Priority {
        self.priority
    }
}

/// Receive buffer for batching
struct ReceiveBuffer {
    capacity: usize,
    completion_tx: oneshot::Sender<Result<(Bytes, SocketAddr)>>,
}

/// Batch operation statistics
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    /// Operations queued for batching
    pub operations_queued: AtomicU64,
    /// Operations executed in batches
    pub operations_executed: AtomicU64,
    /// Number of batches flushed
    pub batches_flushed: AtomicU64,
    /// Total bytes sent in batches
    pub bytes_sent: AtomicU64,
    /// Total bytes received in batches
    pub bytes_received: AtomicU64,
    /// Time spent executing batches (nanoseconds)
    pub batch_execution_time_ns: AtomicU64,
    /// Total operation latency (nanoseconds)
    pub total_latency_ns: AtomicU64,
    /// Receive buffers queued
    pub receive_buffers_queued: AtomicU64,
    /// Receive buffers processed
    pub receive_buffers_processed: AtomicU64,
    /// Receive batches flushed
    pub receive_batches_flushed: AtomicU64,
    /// Receive execution time (nanoseconds)
    pub receive_execution_time_ns: AtomicU64,
}

impl BatchStats {
    /// Calculate batching efficiency (executed / queued)
    pub fn batching_efficiency(&self) -> f64 {
        let queued = self.operations_queued.load(Ordering::Relaxed) as f64;
        let executed = self.operations_executed.load(Ordering::Relaxed) as f64;
        if queued > 0.0 { executed / queued } else { 0.0 }
    }

    /// Calculate average batch size
    pub fn avg_batch_size(&self) -> f64 {
        let executed = self.operations_executed.load(Ordering::Relaxed) as f64;
        let batches = self.batches_flushed.load(Ordering::Relaxed) as f64;
        if batches > 0.0 { executed / batches } else { 0.0 }
    }

    /// Calculate average operation latency (nanoseconds)
    pub fn avg_operation_latency_ns(&self) -> f64 {
        let total_latency = self.total_latency_ns.load(Ordering::Relaxed) as f64;
        let executed_ops = self.operations_executed.load(Ordering::Relaxed) as f64;
        if executed_ops > 0.0 { total_latency / executed_ops } else { 0.0 }
    }

    /// Calculate throughput (bytes per second)
    pub fn throughput_bps(&self, duration: Duration) -> f64 {
        let bytes_sent = self.bytes_sent.load(Ordering::Relaxed) as f64;
        let bytes_received = self.bytes_received.load(Ordering::Relaxed) as f64;
        let total_bytes = bytes_sent + bytes_received;
        let seconds = duration.as_secs_f64();
        if seconds > 0.0 { total_bytes / seconds } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batch_manager_creation() {
        let result = BatchManager::new(32);
        assert!(result.is_ok());

        let manager = result.unwrap();
        let stats = manager.get_stats();
        assert_eq!(stats.operations_queued.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_sender_queue_batching() {
        let config = BatchConfig {
            max_batch_size: 4,
            batch_timeout: Duration::from_millis(100),
            priority_levels: 3,
            adaptive_sizing: false,
            min_batch_size: 2,
            coalescing_window: Duration::from_millis(1),
        };

        let stats = Arc::new(BatchStats::default());
        let queue = SenderQueue::new(config, stats.clone(), Priority::Normal);

        // Queue some operations
        let addr = "127.0.0.1:8080".parse().unwrap();
        let mut receivers = Vec::new();

        for i in 0..3 {
            let data = Bytes::from(format!("message_{}", i));
            let rx = queue.queue_send(data, addr, Priority::Normal);
            receivers.push(rx);
        }

        // Should not flush yet (below max_batch_size)
        assert!(!queue.should_flush());

        // Add one more to trigger flush
        let data = Bytes::from("message_3");
        let rx = queue.queue_send(data, addr, Priority::Normal);
        receivers.push(rx);

        // Now should flush
        assert!(queue.should_flush());

        // Execute flush
        queue.flush().await.unwrap();

        // All operations should complete
        for rx in receivers {
            let result = rx.await.unwrap();
            assert!(result.is_ok());
        }

        let stats = queue.stats;
        assert_eq!(stats.operations_queued.load(Ordering::Relaxed), 4);
        assert_eq!(stats.operations_executed.load(Ordering::Relaxed), 4);
        assert_eq!(stats.batches_flushed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_priority_queue_ordering() {
        let mut queue = PriorityQueue::new();

        // Create operations with different priorities
        let op1 = SendOperation {
            data: Bytes::from("low"),
            addr: "127.0.0.1:8080".parse().unwrap(),
            priority: Priority::Low,
            completion_tx: oneshot::channel().0,
            queued_at: Instant::now(),
        };

        let op2 = SendOperation {
            data: Bytes::from("critical"),
            addr: "127.0.0.1:8080".parse().unwrap(),
            priority: Priority::Critical,
            completion_tx: oneshot::channel().0,
            queued_at: Instant::now(),
        };

        let op3 = SendOperation {
            data: Bytes::from("normal"),
            addr: "127.0.0.1:8080".parse().unwrap(),
            priority: Priority::Normal,
            completion_tx: oneshot::channel().0,
            queued_at: Instant::now(),
        };

        queue.push(op1);
        queue.push(op2);
        queue.push(op3);

        // Should pop in priority order: Critical, Normal, Low
        let first = queue.pop().unwrap();
        assert_eq!(first.priority, Priority::Critical);

        let second = queue.pop().unwrap();
        assert_eq!(second.priority, Priority::Normal);

        let third = queue.pop().unwrap();
        assert_eq!(third.priority, Priority::Low);
    }

    #[test]
    fn test_batch_stats_calculations() {
        let stats = BatchStats::default();
        stats.operations_queued.store(100, Ordering::Relaxed);
        stats.operations_executed.store(95, Ordering::Relaxed);
        stats.batches_flushed.store(10, Ordering::Relaxed);

        let efficiency = stats.batching_efficiency();
        assert!((efficiency - 0.95).abs() < f64::EPSILON);

        let avg_batch_size = stats.avg_batch_size();
        assert!((avg_batch_size - 9.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_receiver_queue_batching() {
        let config = BatchConfig {
            max_batch_size: 3,
            batch_timeout: Duration::from_millis(100),
            priority_levels: 3,
            adaptive_sizing: false,
            min_batch_size: 1,
            coalescing_window: Duration::from_millis(1),
        };

        let stats = Arc::new(BatchStats::default());
        let queue = ReceiverQueue::new(config, stats.clone());

        // Queue receive buffers
        let mut receivers = Vec::new();
        for i in 0..3 {
            let (tx, rx) = oneshot::channel();
            receivers.push(rx);

            let buffer = ReceiveBuffer {
                capacity: 1024,
                completion_tx: tx,
            };

            queue.queue_receive(buffer);
        }

        // Should flush now
        assert!(queue.should_flush());

        // Execute flush
        queue.flush().await.unwrap();

        // All receives should complete
        for rx in receivers {
            let result = rx.await.unwrap();
            assert!(result.is_ok());
        }

        let stats = queue.stats;
        assert_eq!(stats.receive_buffers_queued.load(Ordering::Relaxed), 3);
        assert_eq!(stats.receive_buffers_processed.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn test_adaptive_batch_sizing() {
        let config = BatchConfig {
            max_batch_size: 100,
            batch_timeout: Duration::from_millis(100),
            priority_levels: 3,
            adaptive_sizing: true,
            min_batch_size: 4,
            coalescing_window: Duration::from_millis(1),
        };

        let stats = Arc::new(BatchStats::default());
        let queue = SenderQueue::new(config, stats, Priority::Normal);

        let original_size = queue.adaptive_batch_size.load(Ordering::Relaxed);

        // Simulate high-throughput scenario (fast execution)
        queue.update_adaptive_batch_size(50, Duration::from_micros(10)); // High throughput

        let new_size = queue.adaptive_batch_size.load(Ordering::Relaxed);
        assert!(new_size > original_size); // Should increase batch size

        // Simulate low-throughput scenario (slow execution)
        queue.update_adaptive_batch_size(10, Duration::from_millis(50)); // Low throughput

        let final_size = queue.adaptive_batch_size.load(Ordering::Relaxed);
        assert!(final_size < new_size); // Should decrease batch size
    }
}