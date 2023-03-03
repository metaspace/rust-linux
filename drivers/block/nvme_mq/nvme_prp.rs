use crate::le;
use crate::nvme_driver_defs;
use crate::MappingData;
use crate::NvmeCommand;
use crate::NvmeData;
use core::mem::ManuallyDrop;
use kernel::dma;
use kernel::prelude::*;
use kernel::sync::Arc;
use kernel::types::ScopeGuard;

pub(crate) fn free_prps(
    count: usize,
    pages: &[usize],
    first_dma: u64,
    dma_pool: &Arc<dma::Pool<le<u64>>>,
) {
    let mut dma_addr = first_dma;
    for page in &pages[..count] {
        let prp_list = unsafe {
            dma::CoherentAllocation::<le<u64>, dma::Pool<le<u64>>>::from_parts(
                dma_pool,
                *page,
                dma_addr,
                nvme_driver_defs::NVME_CTRL_PAGE_SIZE / 8,
            )
        };

        dma_addr = prp_list
            .read(nvme_driver_defs::NVME_CTRL_PAGE_SIZE / 8 - 1)
            .unwrap()
            .into();
    }
}

pub(crate) fn setup_prps(
    data: &NvmeData,
    cmd: &mut NvmeCommand,
    md: &mut MappingData,
    mut length: u32,
) -> Result<u32> {
    let mut i = 0;
    let mut sg = &md.sg[i];
    let mut dma_addr = sg.dma_address; // TODO: Use macro.
    let mut dma_len = sg.length; // TODO: Use macro.
    let offset = dma_addr & ((nvme_driver_defs::NVME_CTRL_PAGE_SIZE - 1) as u64);

    let consumed = ((nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u64) - offset) as u32;

    cmd.common.prp1 = dma_addr.into();
    length = length.saturating_sub(consumed);
    if length == 0 {
        return Ok(0);
    }

    dma_len = dma_len.saturating_sub(consumed);
    if dma_len != 0 {
        dma_addr += consumed as u64;
    } else {
        i += 1;
        // TODO: Use sg_next.
        sg = &md.sg[i];
        dma_addr = sg.dma_address;
        dma_len = sg.length;
    }

    if length <= nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u32 {
        cmd.common.prp2 = dma_addr.into();
        return Ok(0);
    }

    let mut prp_list = ManuallyDrop::new(data.dma_pool.try_alloc(true)?);

    cmd.common.prp2 = prp_list.dma_handle.into();
    md.pages[0] = prp_list.first_ptr() as usize;
    struct Data<'a> {
        page_count: usize,
        pages: &'a mut [usize],
        first_dma: u64,
    }
    let mut guard = ScopeGuard::new_with_data(
        Data {
            page_count: 1,
            pages: &mut md.pages,
            first_dma: prp_list.dma_handle,
        },
        |g| {
            free_prps(g.page_count, g.pages, g.first_dma, &data.dma_pool);
        },
    );

    let mut j = 0;
    let mut last_dma_addr = 0;
    loop {
        if j == nvme_driver_defs::NVME_CTRL_PAGE_SIZE / 8 {
            let new_prp_list = ManuallyDrop::new(data.dma_pool.try_alloc(true)?);
            prp_list.write(
                nvme_driver_defs::NVME_CTRL_PAGE_SIZE / 8 - 1,
                &new_prp_list.dma_handle.into(),
            );
            new_prp_list.write(0, &last_dma_addr.into());
            prp_list = new_prp_list;
            let next = guard.page_count;
            guard.pages[next] = prp_list.first_ptr() as usize;
            guard.page_count += 1;
            j = 1;
        }
        last_dma_addr = dma_addr;
        prp_list.write(j, &dma_addr.into());
        j += 1;

        length = length.saturating_sub(nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u32);
        if length == 0 {
            break;
        }

        if dma_len > nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u32 {
            dma_addr += nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u64;
            dma_len -= nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u32;
            continue;
        }

        if dma_len < nvme_driver_defs::NVME_CTRL_PAGE_SIZE as u32 {
            // TODO: Write warning.
            return Err(EIO);
        }

        i += 1;
        // TODO: use sg_next.
        sg = &md.sg[i];
        dma_addr = sg.dma_address;
        dma_len = sg.length;
    }

    Ok(guard.dismiss().page_count as _)
}
