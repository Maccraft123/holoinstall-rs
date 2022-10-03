use aski::Picker;
use hwctl::{
    sysfs::{
        Block,
        SysfsDevice,
    },
};
use gpt::{
    GptConfig,
    partition_types,
};

use std::{
    collections::{BTreeMap, HashMap},
    process::Command,
    path::PathBuf,
    io,
    fs,
};

fn ask_yesno(prompt: &str) -> Result<bool, io::Error> {
    let mut picker = Picker::new(prompt.to_string());
    picker.add_options(vec!["No", "No", "No", "No", "No", "Yes", "No", "No", "No", "No"]).unwrap();
    picker.wait_choice()
        .map(|v| v == "Yes")
}

fn pick_block_dev() -> Result<Block, io::Error> {
    loop {
        let blocks: Vec<Block> = Block::enumerate_all()?
            .into_iter()
            .filter(|v| !v.is_partition().unwrap_or(true))
            .collect();

        let mut blocks_string = Vec::new();
        let mut block_names = HashMap::new();

        for dev in blocks {
            if let Some(name) = dev.fancy_name() {
                block_names.insert(name.clone(), dev);
                blocks_string.push(name);
            } else if let Some(name) = dev.dev_path() {
                block_names.insert(name.display().to_string(), dev);
                blocks_string.push(name.display().to_string());
            };
        }

        let mut picker = Picker::new("Pick the target device".to_string());
        picker.add_options(blocks_string).unwrap();

        let response = picker.wait_choice()?;
        if let Some((key, value)) = block_names.remove_entry(&response) {
            let mut you_sure = Picker::new(format!("Are you sure you want to use {}?", key));
            you_sure.add_options(vec!["No", "No", "No", "No", "No", "Yes", "No", "No", "No", "No"]).unwrap();
            
            let response = you_sure.wait_choice()?;
            if response == "Yes" {
                return Ok(value);
            }
        }
    }
}

fn find_esp(device: &Block) -> Result<u32, io::Error> {
    const ESP_GUID: &'static str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";

    let gpt_cfg = GptConfig::new().writable(false);
    let disk = gpt_cfg.open(device.dev_path().unwrap())?;

    let mut tmp = disk.partitions().into_iter()
        .filter(|(_, v)| v.part_type_guid.guid == ESP_GUID)
        .map(|(k, _)| *k)
        .collect::<Vec<u32>>();

    Ok(tmp.swap_remove(0))
}

fn main() -> Result<(), io::Error> {
    let dualboot = ask_yesno("Do you want to install this OS alongside existing setup?")?;
    let blockdev = pick_block_dev()?;
    let writable = true;

    let gpt_cfg = GptConfig::new().writable(writable);
    let mut disk = gpt_cfg.open(blockdev.dev_path().unwrap())?;

    let root_part_idx;
    let esp_part_idx;

    if dualboot {
        esp_part_idx = find_esp(&blockdev)?;
        let mut freespace_vec = disk.find_free_sectors();
        freespace_vec.sort_by(|(_, a), (_, b)| a.cmp(b));

        let (_, length_sectors) = freespace_vec.last().unwrap();
        // 16 gb * 1024 mb/gb * 1024 kb/mb * 2 sectors/kb
        let length_gb = length_sectors/1024/1024/2;
        if length_gb < 16 {
            panic!("Could not find any free space bigger than 16GB!");
        } else if length_gb < 64 {
            let cont = ask_yesno("This device has less than 64GB of free space, Are you sure you picked correct device?")?;
            if !cont {
                return Ok(())
            }
        }

        // round down a bit for some wiggle room
        root_part_idx = disk.add_partition("HoloFork", length_sectors * 500, partition_types::LINUX_ROOT_X64, 0, None)?;
    } else {
        // clear out partition table
        disk.update_partitions(BTreeMap::new())?;
        
        // 1gb * 1000 mb/gb * 1000 kb/gb * 1000 b/kb
        esp_part_idx = disk.add_partition("ESP", 1 * 1000 * 1000 * 1000, partition_types::EFI, 0, None)?;

        let mut freespace_vec = disk.find_free_sectors();
        freespace_vec.sort_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap());
        let (_, length_sectors) = freespace_vec.last().unwrap();
        root_part_idx = disk.add_partition("HoloFork", length_sectors * 500, partition_types::LINUX_ROOT_X64, 0, None)?;
    }

    if writable {
        disk.write().unwrap();
    }

    // now that partitioning is over, we can do the actual installation, after mounting
    let mut partitions = blockdev.partitions();
    partitions.sort_by(|a, b| a.dev_path().unwrap().display().to_string().cmp(&b.dev_path().unwrap().display().to_string()));

    let esp_part = &partitions[(esp_part_idx - 1) as usize];
    let root_part = &partitions[(root_part_idx - 1) as usize];

    if !dualboot {
        Command::new("mkfs")
            .args(["-t", "vfat"])
            .arg(&esp_part.dev_path().unwrap())
            .status()
            .expect("Failed to format esp partition");
    }

    Command::new("mkfs")
        .args(["-t", "btrfs"])
        .arg("-f")
        .arg(&root_part.dev_path().unwrap())
        .status()
        .expect("Failed to format root partition");

    // aaand mount
    let root_path = PathBuf::from("/tmp/holoinstall/mnt/");
    let esp_path = PathBuf::from("/tmp/holoinstall/mnt/boot/");

    fs::create_dir_all(&root_path)?;
    Command::new("mount")
        .args(["-t", "btrfs"])
        .args(["-o", "subvol=/,compress-force=zstd:1,discard,noatime,nodiratime"])
        .args([&root_part.dev_path().unwrap(), &root_path])
        .output()
        .expect("Failed to mount root partition");

    fs::create_dir_all(&esp_path)?;
    Command::new("mount")
        .args(["-t", "vfat"])
        .args([&esp_part.dev_path().unwrap(), &esp_path])
        .status()
        .expect("Failed to mount esp partition");

    Command::new("holoinstall")
        .arg("/tmp/holoinstall/mnt")
        .status()
        .expect("Failed to run installation script");

    Ok(())
}
