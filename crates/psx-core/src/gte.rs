// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Prodigy75000

//! COP2: the Geometry Transformation Engine.
//!
//! **Status: register file only.** Moves in and out of the 32 data and 32
//! control registers work, so code that saves/restores GTE context across a
//! context switch behaves; the 15 commands (RTPS, NCLIP, MVMVA, ...) are
//! counted and otherwise ignored. Nothing renders yet, so a wrong GTE result
//! is invisible; that changes the moment the GPU lands, and the commands are
//! the next thing after it.
//!
//! The register file is serialized into save states from day one so that adding
//! the commands later does not move the state layout around.

#[derive(Clone)]
pub struct Gte {
    /// cop2r0..31: vectors, colours, the ordering-table entry, MAC/IR results.
    pub data: [u32; 32],
    /// cop2r32..63: the rotation/light/colour matrices, translation vectors,
    /// projection constants and FLAG.
    pub control: [u32; 32],
    /// How many COP2 commands were issued and dropped. The harnesses print it
    /// so "the GTE is not implemented" is a number, not a memory.
    pub unimplemented_commands: u64,
}

impl Default for Gte {
    fn default() -> Self {
        Gte::new()
    }
}

impl Gte {
    pub fn new() -> Gte {
        Gte {
            data: [0; 32],
            control: [0; 32],
            unimplemented_commands: 0,
        }
    }

    /// `MFC2`: read a data register.
    pub fn read_data(&self, index: u32) -> u32 {
        self.data[(index & 31) as usize]
    }

    /// `MTC2`: write a data register.
    pub fn write_data(&mut self, index: u32, val: u32) {
        self.data[(index & 31) as usize] = val;
    }

    /// `CFC2`: read a control register.
    pub fn read_control(&self, index: u32) -> u32 {
        self.control[(index & 31) as usize]
    }

    /// `CTC2`: write a control register.
    pub fn write_control(&mut self, index: u32, val: u32) {
        self.control[(index & 31) as usize] = val;
    }

    /// A COP2 command (`cop2 imm25`). Not implemented; counted so the gap is
    /// measurable from the outside.
    pub fn command(&mut self, _op: u32) {
        self.unimplemented_commands += 1;
    }
}
