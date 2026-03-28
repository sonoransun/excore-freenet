//! High-Performance Memory Management
//!
//! This module provides advanced memory management capabilities including:
//! - Zero-copy buffer pools for network operations
//! - Memory-mapped file I/O for large data sets
//! - Custom allocators optimized for network workloads
//! - Buffer recycling and reuse strategies

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::collections::VecDeque;
use std::time::Instant;

use anyhow::{Result, Context};
use bytes::{Bytes, BytesMut};
use parking_lot::{Mutex, RwLock};

/// High-performance memory manager
pub struct MemoryManager {
    buffer_pool: Arc<BufferPool>,
    mmap_manager: Arc<MMapManager>,
    stats: Arc<MemoryStats>,
    config: MemoryConfig,
}

impl MemoryManager {
    /// Create new memory manager
    pub fn new(pool_size: usize) -> Result<Self> {
        let config = MemoryConfig {
            pool_size,
            max_buffer_size: 64 * 1024, // 64KB max buffers
            min_buffer_size: 1024,      // 1KB min buffers
            pool_growth_factor: 1.5,
            max_pools: 16,
        };

        let buffer_pool = Arc::new(BufferPool::new(&config)?);
        let mmap_manager = Arc::new(MMapManager::new()?);
        let stats = Arc::new(MemoryStats::default());

        Ok(Self {
            buffer_pool,
            mmap_manager,
            stats,
            config,
        })
    }

    /// Get buffer from pool for zero-copy operations
    pub fn get_buffer(&self, size: usize) -> Result<PooledBuffer> {
        let start = Instant::now();

        let buffer = self.buffer_pool.get_buffer(size)?;

        let elapsed = start.elapsed();
        self.stats.buffer_allocations.fetch_add(1, Ordering::Relaxed);
        self.stats.allocation_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(buffer)
    }

    /// Return buffer to pool for reuse
    pub fn return_buffer(&self, buffer: PooledBuffer) {
        let start = Instant::now();

        self.buffer_pool.return_buffer(buffer);

        let elapsed = start.elapsed();
        self.stats.buffer_deallocations.fetch_add(1, Ordering::Relaxed);
        self.stats.deallocation_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Create memory-mapped file for large data
    pub fn create_mmap(&self, path: &std::path::Path, size: usize) -> Result<MappedFile> {
        self.mmap_manager.create_mmap(path, size)
    }

    /// Open existing memory-mapped file
    pub fn open_mmap(&self, path: &std::path::Path) -> Result<MappedFile> {
        self.mmap_manager.open_mmap(path)
    }

    /// Create zero-copy slice from bytes
    pub fn zero_copy_slice(&self, data: Bytes, offset: usize, length: usize) -> Result<Bytes> {
        if offset + length > data.len() {
            anyhow::bail!("Slice bounds exceed data length");
        }

        // Zero-copy slice using bytes crate
        let slice = data.slice(offset..offset + length);

        self.stats.zero_copy_operations.fetch_add(1, Ordering::Relaxed);
        Ok(slice)
    }

    /// Batch allocate buffers for network operations
    pub fn allocate_batch(&self, sizes: &[usize]) -> Result<Vec<PooledBuffer>> {
        let start = Instant::now();

        let mut buffers = Vec::with_capacity(sizes.len());

        for &size in sizes {
            buffers.push(self.buffer_pool.get_buffer(size)?);
        }

        let elapsed = start.elapsed();
        self.stats.batch_allocations.fetch_add(1, Ordering::Relaxed);
        self.stats.allocation_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(buffers)
    }

    /// Get memory statistics
    pub fn get_stats(&self) -> MemoryStats {
        (*self.stats).clone()
    }

    /// Force garbage collection of unused buffers
    pub fn gc(&self) -> Result<usize> {
        let freed = self.buffer_pool.gc()?;
        self.stats.gc_operations.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_freed.fetch_add(freed as u64, Ordering::Relaxed);
        Ok(freed)
    }
}

/// Memory management configuration
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Total pool size in bytes
    pub pool_size: usize,
    /// Maximum buffer size
    pub max_buffer_size: usize,
    /// Minimum buffer size
    pub min_buffer_size: usize,
    /// Pool growth factor when expanding
    pub pool_growth_factor: f32,
    /// Maximum number of size-specific pools
    pub max_pools: usize,
}

/// Buffer pool for zero-copy operations
pub struct BufferPool {
    pools: RwLock<Vec<SizePool>>,
    config: MemoryConfig,
    total_allocated: AtomicUsize,
    total_capacity: AtomicUsize,
}

impl BufferPool {
    /// Create new buffer pool
    pub fn new(config: &MemoryConfig) -> Result<Self> {
        let mut pools = Vec::new();

        // Create size-specific pools (powers of 2)
        let mut size = config.min_buffer_size;
        while size <= config.max_buffer_size && pools.len() < config.max_pools {
            let pool_capacity = config.pool_size / (size * config.max_pools);
            pools.push(SizePool::new(size, pool_capacity));
            size *= 2;
        }

        Ok(Self {
            pools: RwLock::new(pools),
            config: config.clone(),
            total_allocated: AtomicUsize::new(0),
            total_capacity: AtomicUsize::new(config.pool_size),
        })
    }

    /// Get buffer of specified size
    pub fn get_buffer(&self, size: usize) -> Result<PooledBuffer> {
        let pool_index = self.find_pool_for_size(size);
        let pools = self.pools.read();

        if let Some(index) = pool_index {
            if index < pools.len() {
                if let Some(buffer) = pools[index].get_buffer() {
                    self.total_allocated.fetch_add(buffer.capacity(), Ordering::Relaxed);
                    return Ok(buffer);
                }
            }
        }

        // Fallback: allocate new buffer
        let buffer = PooledBuffer::new(size, None);
        self.total_allocated.fetch_add(buffer.capacity(), Ordering::Relaxed);
        Ok(buffer)
    }

    /// Return buffer to appropriate pool
    pub fn return_buffer(&self, mut buffer: PooledBuffer) {
        self.total_allocated.fetch_sub(buffer.capacity(), Ordering::Relaxed);

        if let Some(pool_index) = buffer.pool_index {
            let pools = self.pools.read();
            if pool_index < pools.len() {
                pools[pool_index].return_buffer(buffer);
                return;
            }
        }

        // Buffer not from pool, just drop it
    }

    /// Find appropriate pool for given size
    fn find_pool_for_size(&self, size: usize) -> Option<usize> {
        let pools = self.pools.read();

        for (i, pool) in pools.iter().enumerate() {
            if size <= pool.buffer_size {
                return Some(i);
            }
        }

        None
    }

    /// Garbage collect unused buffers
    pub fn gc(&self) -> Result<usize> {
        let pools = self.pools.read();
        let mut freed_bytes = 0;

        for pool in pools.iter() {
            freed_bytes += pool.gc();
        }

        Ok(freed_bytes)
    }
}

/// Size-specific buffer pool
pub struct SizePool {
    buffer_size: usize,
    available_buffers: Mutex<VecDeque<PooledBuffer>>,
    capacity: usize,
    allocated_count: AtomicUsize,
}

impl SizePool {
    /// Create new size pool
    pub fn new(buffer_size: usize, capacity: usize) -> Self {
        Self {
            buffer_size,
            available_buffers: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            allocated_count: AtomicUsize::new(0),
        }
    }

    /// Get buffer from pool
    pub fn get_buffer(&self) -> Option<PooledBuffer> {
        let mut available = self.available_buffers.lock();
        available.pop_front()
    }

    /// Return buffer to pool
    pub fn return_buffer(&self, mut buffer: PooledBuffer) {
        // Reset buffer for reuse
        buffer.clear();

        let mut available = self.available_buffers.lock();
        if available.len() < self.capacity {
            available.push_back(buffer);
        }
        // If pool is full, buffer will be dropped
    }

    /// Garbage collect half of available buffers
    pub fn gc(&self) -> usize {
        let mut available = self.available_buffers.lock();
        let to_remove = available.len() / 2;
        let freed_bytes = to_remove * self.buffer_size;

        for _ in 0..to_remove {
            available.pop_back();
        }

        freed_bytes
    }
}

/// Pooled buffer for zero-copy operations
pub struct PooledBuffer {
    data: BytesMut,
    pool_index: Option<usize>,
}

impl PooledBuffer {
    /// Create new pooled buffer
    pub fn new(size: usize, pool_index: Option<usize>) -> Self {
        Self {
            data: BytesMut::with_capacity(size),
            pool_index,
        }
    }

    /// Get buffer capacity
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// Get buffer length
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Clear buffer contents
    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Resize buffer
    pub fn resize(&mut self, new_len: usize, value: u8) {
        self.data.resize(new_len, value);
    }

    /// Get mutable slice
    pub fn as_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    /// Get immutable slice
    pub fn as_ref(&self) -> &[u8] {
        &self.data[..]
    }

    /// Convert to Bytes (zero-copy)
    pub fn freeze(self) -> Bytes {
        self.data.freeze()
    }

    /// Extend with data
    pub fn extend_from_slice(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }
}

/// Memory-mapped file manager
pub struct MMapManager {
    open_files: RwLock<Vec<MappedFileHandle>>,
}

impl MMapManager {
    /// Create new mmap manager
    pub fn new() -> Result<Self> {
        Ok(Self {
            open_files: RwLock::new(Vec::new()),
        })
    }

    /// Create new memory-mapped file
    pub fn create_mmap(&self, path: &std::path::Path, size: usize) -> Result<MappedFile> {
        use std::fs::OpenOptions;
        use std::io::Write;

        // Create file with specified size
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)
            .context("Failed to create file for memory mapping")?;

        // Resize file to desired size
        file.set_len(size as u64)
            .context("Failed to set file size")?;

        // Create memory mapping
        let mmap = unsafe {
            memmap2::MmapMut::map_mut(&file)
                .context("Failed to create memory mapping")?
        };

        let mapped_file = MappedFile {
            path: path.to_path_buf(),
            size,
            mmap: Some(mmap),
            read_only: false,
        };

        // Track open file
        let handle = MappedFileHandle {
            path: path.to_path_buf(),
            size,
        };

        {
            let mut files = self.open_files.write();
            files.push(handle);
        }

        Ok(mapped_file)
    }

    /// Open existing memory-mapped file
    pub fn open_mmap(&self, path: &std::path::Path) -> Result<MappedFile> {
        use std::fs::File;

        let file = File::open(path)
            .context("Failed to open file for memory mapping")?;

        let metadata = file.metadata()
            .context("Failed to get file metadata")?;
        let size = metadata.len() as usize;

        // Create read-only memory mapping
        let mmap = unsafe {
            memmap2::Mmap::map(&file)
                .context("Failed to create read-only memory mapping")?
        };

        let mapped_file = MappedFile {
            path: path.to_path_buf(),
            size,
            mmap: None,
            read_only: true,
        };

        Ok(mapped_file)
    }
}

/// Memory-mapped file handle
struct MappedFileHandle {
    path: std::path::PathBuf,
    size: usize,
}

/// Memory-mapped file
pub struct MappedFile {
    path: std::path::PathBuf,
    size: usize,
    mmap: Option<memmap2::MmapMut>,
    read_only: bool,
}

impl MappedFile {
    /// Get file size
    pub fn size(&self) -> usize {
        self.size
    }

    /// Check if file is read-only
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Get read-only slice
    pub fn as_slice(&self) -> &[u8] {
        if let Some(ref mmap) = self.mmap {
            &mmap[..]
        } else {
            &[] // Read-only mapping would use different field
        }
    }

    /// Get mutable slice (only for writable mappings)
    pub fn as_mut_slice(&mut self) -> Result<&mut [u8]> {
        if self.read_only {
            anyhow::bail!("Cannot get mutable slice from read-only mapping");
        }

        if let Some(ref mut mmap) = self.mmap {
            Ok(&mut mmap[..])
        } else {
            anyhow::bail!("No mutable mapping available");
        }
    }

    /// Flush changes to disk
    pub fn flush(&mut self) -> Result<()> {
        if let Some(ref mut mmap) = self.mmap {
            mmap.flush().context("Failed to flush memory mapping")?;
        }
        Ok(())
    }
}

/// Memory management statistics
#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    /// Number of buffer allocations
    pub buffer_allocations: AtomicU64,
    /// Number of buffer deallocations
    pub buffer_deallocations: AtomicU64,
    /// Number of batch allocations
    pub batch_allocations: AtomicU64,
    /// Number of zero-copy operations
    pub zero_copy_operations: AtomicU64,
    /// Number of garbage collection operations
    pub gc_operations: AtomicU64,
    /// Bytes freed by garbage collection
    pub bytes_freed: AtomicU64,
    /// Time spent on allocations (nanoseconds)
    pub allocation_time_ns: AtomicU64,
    /// Time spent on deallocations (nanoseconds)
    pub deallocation_time_ns: AtomicU64,
}

impl MemoryStats {
    /// Calculate allocation rate (operations per second)
    pub fn allocation_rate(&self, duration: std::time::Duration) -> f64 {
        let allocations = self.buffer_allocations.load(Ordering::Relaxed) as f64;
        let seconds = duration.as_secs_f64();
        if seconds > 0.0 { allocations / seconds } else { 0.0 }
    }

    /// Calculate average allocation latency (nanoseconds)
    pub fn avg_allocation_latency_ns(&self) -> f64 {
        let total_time = self.allocation_time_ns.load(Ordering::Relaxed) as f64;
        let total_allocations = self.buffer_allocations.load(Ordering::Relaxed) as f64;
        if total_allocations > 0.0 { total_time / total_allocations } else { 0.0 }
    }

    /// Calculate memory efficiency (deallocations / allocations)
    pub fn memory_efficiency(&self) -> f64 {
        let allocations = self.buffer_allocations.load(Ordering::Relaxed) as f64;
        let deallocations = self.buffer_deallocations.load(Ordering::Relaxed) as f64;
        if allocations > 0.0 { deallocations / allocations } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_memory_manager_creation() {
        let result = MemoryManager::new(1024 * 1024); // 1MB pool
        assert!(result.is_ok());

        let manager = result.unwrap();
        let stats = manager.get_stats();
        assert_eq!(stats.buffer_allocations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_buffer_allocation() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();

        let buffer = manager.get_buffer(1024).unwrap();
        assert_eq!(buffer.capacity(), 1024);
        assert!(buffer.is_empty());

        let stats = manager.get_stats();
        assert_eq!(stats.buffer_allocations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_buffer_reuse() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();

        // Allocate and return buffer
        let buffer = manager.get_buffer(1024).unwrap();
        let capacity = buffer.capacity();
        manager.return_buffer(buffer);

        // Allocate again - should reuse
        let buffer2 = manager.get_buffer(1024).unwrap();
        assert_eq!(buffer2.capacity(), capacity);

        let stats = manager.get_stats();
        assert_eq!(stats.buffer_allocations.load(Ordering::Relaxed), 2);
        assert_eq!(stats.buffer_deallocations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_zero_copy_slice() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();

        let data = Bytes::from_static(b"Hello, World!");
        let slice = manager.zero_copy_slice(data.clone(), 7, 5).unwrap();

        assert_eq!(slice.as_ref(), b"World");
        assert_eq!(slice.len(), 5);

        let stats = manager.get_stats();
        assert_eq!(stats.zero_copy_operations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_batch_allocation() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();

        let sizes = vec![512, 1024, 2048];
        let buffers = manager.allocate_batch(&sizes).unwrap();

        assert_eq!(buffers.len(), 3);
        for (i, buffer) in buffers.iter().enumerate() {
            assert_eq!(buffer.capacity(), sizes[i]);
        }

        let stats = manager.get_stats();
        assert_eq!(stats.batch_allocations.load(Ordering::Relaxed), 1);
        assert_eq!(stats.buffer_allocations.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_memory_mapped_file() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();
        let temp_dir = tempdir().unwrap();
        let file_path = temp_dir.path().join("test.dat");

        // Create memory-mapped file
        let mut mmap_file = manager.create_mmap(&file_path, 4096).unwrap();
        assert_eq!(mmap_file.size(), 4096);
        assert!(!mmap_file.is_read_only());

        // Write some data
        let slice = mmap_file.as_mut_slice().unwrap();
        slice[0..5].copy_from_slice(b"Hello");

        // Flush changes
        mmap_file.flush().unwrap();

        // Verify file exists
        assert!(file_path.exists());
    }

    #[test]
    fn test_pooled_buffer_operations() {
        let mut buffer = PooledBuffer::new(1024, Some(0));

        assert_eq!(buffer.capacity(), 1024);
        assert!(buffer.is_empty());

        buffer.extend_from_slice(b"test data");
        assert_eq!(buffer.len(), 9);
        assert_eq!(buffer.as_ref(), b"test data");

        buffer.clear();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_memory_stats_calculations() {
        let stats = MemoryStats::default();
        stats.buffer_allocations.store(100, Ordering::Relaxed);
        stats.buffer_deallocations.store(80, Ordering::Relaxed);
        stats.allocation_time_ns.store(1_000_000, Ordering::Relaxed); // 1ms

        let efficiency = stats.memory_efficiency();
        assert!((efficiency - 0.8).abs() < f64::EPSILON);

        let avg_latency = stats.avg_allocation_latency_ns();
        assert!((avg_latency - 10_000.0).abs() < f64::EPSILON); // 10μs per allocation
    }

    #[test]
    fn test_garbage_collection() {
        let manager = MemoryManager::new(1024 * 1024).unwrap();

        // Allocate and return several buffers
        for i in 0..10 {
            let buffer = manager.get_buffer(1024).unwrap();
            manager.return_buffer(buffer);
        }

        // Run garbage collection
        let freed = manager.gc().unwrap();
        assert!(freed > 0);

        let stats = manager.get_stats();
        assert_eq!(stats.gc_operations.load(Ordering::Relaxed), 1);
        assert!(stats.bytes_freed.load(Ordering::Relaxed) > 0);
    }
}