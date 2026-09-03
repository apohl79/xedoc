const LARGE_TRANSIENT_ALLOCATION_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn release_after_large_write(serialized_bytes: usize) {
    if serialized_bytes < LARGE_TRANSIENT_ALLOCATION_BYTES {
        return;
    }
    release_unused_allocator_memory();
}

#[cfg(target_os = "macos")]
fn release_unused_allocator_memory() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }

    // SAFETY: A null zone asks the system allocator to inspect every registered zone. A zero
    // goal is the documented request to release as many unused pages as practical.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn release_unused_allocator_memory() {}
