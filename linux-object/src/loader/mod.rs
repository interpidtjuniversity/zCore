use {
    crate::error::LxResult,
    crate::fs::INodeExt,
    alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec},
    rcore_fs::vfs::INode,
    xmas_elf::ElfFile,
    zircon_object::{util::elf_loader::*, vm::*, ZxError},
};

mod abi;

/// Linux ELF Program Loader.
pub struct LinuxElfLoader {
    pub syscall_entry: usize,
    pub stack_pages: usize,
    pub root_inode: Arc<dyn INode>,
}

impl LinuxElfLoader {
    pub fn load(
        &self,
        vmar: &Arc<VmAddressRegion>,
        data: &[u8],
        mut args: Vec<String>,
        envs: Vec<String>,
    ) -> LxResult<(VirtAddr, VirtAddr)> {
        let elf = ElfFile::new(data).map_err(|_| ZxError::INVALID_ARGS)?;
        if let Ok(interp) = elf.get_interpreter() {
            info!("interp: {:?}", interp);
            let inode = self.root_inode.lookup(interp)?;
            let data = inode.read_as_vec()?;
            args.insert(0, interp.into());
            return self.load(vmar, &data, args, envs);
        }

        let size = elf.load_segment_size();
        let image_vmar = vmar.allocate(None, size, VmarFlags::CAN_MAP_RXW, PAGE_SIZE)?;
        let base = image_vmar.addr();
        let vmo = image_vmar.load_from_elf(&elf)?;
        let entry = base + elf.header.pt2.entry_point() as usize;

        // fill syscall entry
        if let Some(offset) = elf.get_symbol_address("rcore_syscall_entry") {
            vmo.write(offset as usize, &self.syscall_entry.to_ne_bytes())?;
        }

        elf.relocate(base).map_err(|_| ZxError::INVALID_ARGS)?;

        let stack_vmo = VmObject::new_paged(self.stack_pages);
        let flags = MMUFlags::READ | MMUFlags::WRITE;
        let stack_bottom = vmar.map(None, stack_vmo.clone(), 0, stack_vmo.len(), flags)?;
        let mut sp = stack_bottom + stack_vmo.len();

        let info = abi::ProcInitInfo {
            args,
            envs,
            auxv: {
                let mut map = BTreeMap::new();
                map.insert(abi::AT_BASE, base);
                map.insert(abi::AT_PHDR, base + elf.header.pt2.ph_offset() as usize);
                map.insert(abi::AT_PHENT, elf.header.pt2.ph_entry_size() as usize);
                map.insert(abi::AT_PHNUM, elf.header.pt2.ph_count() as usize);
                map.insert(abi::AT_PAGESZ, PAGE_SIZE);
                map
            },
        };
        #[allow(unsafe_code)]
        unsafe {
            sp = info.push_at(sp);
        }

        Ok((entry, sp))
    }
}
