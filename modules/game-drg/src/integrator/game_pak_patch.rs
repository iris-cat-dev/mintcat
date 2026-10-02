use mintcat_integrator_core::uasset_utils::splice::{
    extract_tracked_statements, inject_tracked_statements, walk, AssetVersion, TrackedStatement,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io;
use std::io::{Read, Seek};
use tracing::info;
use unreal_asset::properties::object_property::{
    SoftObjectPath, SoftObjectProperty, TopLevelAssetPath,
};
use unreal_asset::properties::{Ancestry, FName};
use unreal_asset::{
    exports::ExportBaseTrait,
    flags::EObjectFlags,
    kismet::{
        EExprToken, ExByteConst, ExCallMath, ExLet, ExLetObj, ExLocalVariable, ExRotationConst,
        ExSelf, ExSoftObjectConst, ExStringConst, ExVectorConst, FieldPath, KismetPropertyPointer,
    },
    kismet::{ExFalse, KismetExpression},
    types::vector::Vector,
    types::PackageIndex,
    Asset,
};

pub static PCB_PATH: &str = "FSD/Content/Game/BP_PlayerControllerBase";
pub static PATCH_PATHS: [&str; 6] = [
    "FSD/Content/Game/BP_GameInstance",
    "FSD/Content/Game/SpaceRig/BP_PlayerController_SpaceRig",
    "FSD/Content/Game/StartMenu/Bp_StartMenu_PlayerController",
    "FSD/Content/UI/Menu_DeepDives/ITM_DeepDives_Join",
    "FSD/Content/UI/Menu_ServerList/_MENU_ServerList",
    "FSD/Content/UI/Menu_ServerList/WND_JoiningModded",
];
pub static ESCAPE_MENU_PATH: &str = "FSD/Content/UI/Menu_EscapeMenu/MENU_EscapeMenu";
pub static MODDING_TAB_PATH: &str = "FSD/Content/UI/Menu_EscapeMenu/Modding/MENU_Modding";
pub static SERVER_LIST_ENTRY_PATH: &str = "FSD/Content/UI/Menu_ServerList/ITM_ServerList_Entry";
pub static MINT_PATH: &str = "/Game/_AssemblyStorm/ModIntegration/MI_SpawnMods.MI_SpawnMods_C";

pub fn get_deferred_paths() -> Vec<&'static str> {
    let mut paths = vec![
        PCB_PATH,
        ESCAPE_MENU_PATH,
        MODDING_TAB_PATH,
        SERVER_LIST_ENTRY_PATH,
    ];
    paths.extend(PATCH_PATHS);
    paths
}

type ImportChain<'a> = Vec<Import<'a>>;

struct Import<'a> {
    class_package: &'a str,
    class_name: &'a str,
    object_name: &'a str,
}

impl<'a> Import<'a> {
    fn new(class_package: &'a str, class_name: &'a str, object_name: &'a str) -> Import<'a> {
        Import {
            class_package,
            class_name,
            object_name,
        }
    }
}

fn get_import<R: Read + Seek>(asset: &mut Asset<R>, import: ImportChain) -> PackageIndex {
    let mut pi = PackageIndex::new(0);
    for i in import {
        let ai = &asset
            .imports
            .iter()
            .enumerate()
            .find(|(_, ai)| {
                ai.class_package.get_content(|n| n == i.class_package)
                    && ai.class_name.get_content(|n| n == i.class_name)
                    && ai.object_name.get_content(|n| n == i.object_name)
                    && ai.outer_index == pi
            })
            .map(|(index, _)| PackageIndex::from_import(index as i32).unwrap());
        pi = ai.unwrap_or_else(|| {
            let new_import = unreal_asset::Import::new(
                asset.add_fname(i.class_package),
                asset.add_fname(i.class_name),
                pi,
                asset.add_fname(i.object_name),
                false,
            );
            asset.add_import(new_import)
        });
    }
    pi
}

fn find_export_named<'a, R: io::Read + io::Seek>(
    asset: &'a mut Asset<R>,
    name: &'a str,
) -> Option<&'a mut unreal_asset::exports::normal_export::NormalExport> {
    for export in &mut asset.asset_data.exports {
        if let unreal_asset::exports::Export::NormalExport(export) = export {
            if export.base_export.object_name.get_content(|n| n == name) {
                return Some(export);
            }
        }
    }
    None
}
fn find_array_property_named<'a>(
    export: &'a mut unreal_asset::exports::normal_export::NormalExport,
    name: &'a str,
) -> Option<(
    usize,
    &'a mut unreal_asset::properties::array_property::ArrayProperty,
)> {
    for (i, prop) in &mut export.properties.iter_mut().enumerate() {
        if let unreal_asset::properties::Property::ArrayProperty(prop) = prop {
            if prop.name.get_content(|n| n == name) {
                return Some((i, prop));
            }
        }
    }
    None
}
#[allow(dead_code)]
fn find_struct_property_named<'a>(
    export: &'a mut unreal_asset::exports::normal_export::NormalExport,
    name: &'a str,
) -> Option<(
    usize,
    &'a mut unreal_asset::properties::struct_property::StructProperty,
)> {
    for (i, prop) in &mut export.properties.iter_mut().enumerate() {
        if let unreal_asset::properties::Property::StructProperty(prop) = prop {
            if prop.name.get_content(|n| n == name) {
                return Some((i, prop));
            }
        }
    }
    None
}

/// "it's only 3 instructions"
/// "how much boilerplate could there possibly be"
pub fn hook_pcb<R: Read + Seek>(asset: &mut Asset<R>) -> Result<()> {
    let transform = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/CoreUObject"),
            Import::new("/Script/CoreUObject", "ScriptStruct", "Transform"),
        ],
    );
    let actor = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/Engine"),
            Import::new("/Script/CoreUObject", "Class", "Actor"),
        ],
    );
    let load_class = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/Engine"),
            Import::new("/Script/CoreUObject", "Class", "KismetSystemLibrary"),
            Import::new("/Script/CoreUObject", "Function", "LoadClassAsset_Blocking"),
        ],
    );
    let make_transform = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/Engine"),
            Import::new("/Script/CoreUObject", "Class", "KismetMathLibrary"),
            Import::new("/Script/CoreUObject", "Function", "MakeTransform"),
        ],
    );
    let begin_spawning = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/Engine"),
            Import::new("/Script/CoreUObject", "Class", "GameplayStatics"),
            Import::new(
                "/Script/CoreUObject",
                "Function",
                "BeginDeferredActorSpawnFromClass",
            ),
        ],
    );
    let finish_spawning = get_import(
        asset,
        vec![
            Import::new("/Script/CoreUObject", "Package", "/Script/Engine"),
            Import::new("/Script/CoreUObject", "Class", "GameplayStatics"),
            Import::new("/Script/CoreUObject", "Function", "FinishSpawningActor"),
        ],
    );
    let ex_transform = ExCallMath {
        token: EExprToken::ExCallMath,
        stack_node: make_transform,
        parameters: vec![
            ExVectorConst {
                token: EExprToken::ExVectorConst,
                value: Vector::new(0f64.into(), 0f64.into(), 0f64.into()),
            }
            .into(),
            ExRotationConst {
                token: EExprToken::ExVectorConst,
                rotator: Vector::new(0f64.into(), 0f64.into(), 0f64.into()),
            }
            .into(),
            ExVectorConst {
                token: EExprToken::ExVectorConst,
                value: Vector::new(1f64.into(), 1f64.into(), 1f64.into()),
            }
            .into(),
        ],
    };
    let prop_class_name = asset.add_fname("begin_spawn");
    let prop_class = unreal_asset::fproperty::FObjectProperty {
        generic_property: unreal_asset::fproperty::FGenericProperty {
            name: prop_class_name.clone(),
            flags: EObjectFlags::RF_PUBLIC,
            array_dim: unreal_asset::enums::EArrayDim::TArray,
            element_size: 8,
            property_flags: unreal_asset::flags::EPropertyFlags::CPF_NONE,
            rep_index: 0,
            rep_notify_func: asset.add_fname("None"),
            blueprint_replication_condition: unreal_asset::enums::ELifetimeCondition::CondNone,
            serialized_type: Some(asset.add_fname("ClassProperty")),
        },
        property_class: actor,
    };
    let prop_transform_name = asset.add_fname("transform");
    let prop_transform = unreal_asset::fproperty::FStructProperty {
        generic_property: unreal_asset::fproperty::FGenericProperty {
            name: prop_transform_name.clone(),
            flags: EObjectFlags::RF_PUBLIC,
            array_dim: unreal_asset::enums::EArrayDim::TArray,
            element_size: 48,
            property_flags: unreal_asset::flags::EPropertyFlags::CPF_NONE,
            rep_index: 0,
            rep_notify_func: asset.add_fname("None"),
            blueprint_replication_condition: unreal_asset::enums::ELifetimeCondition::CondNone,
            serialized_type: Some(asset.add_fname("StructProperty")),
        },
        struct_value: transform,
    };
    let prop_begin_spawn_name = asset.add_fname("begin_spawn");
    let prop_begin_spawn = unreal_asset::fproperty::FObjectProperty {
        generic_property: unreal_asset::fproperty::FGenericProperty {
            name: prop_begin_spawn_name.clone(),
            flags: EObjectFlags::RF_PUBLIC,
            array_dim: unreal_asset::enums::EArrayDim::TArray,
            element_size: 8,
            property_flags: unreal_asset::flags::EPropertyFlags::CPF_NONE,
            rep_index: 0,
            rep_notify_func: asset.add_fname("None"),
            blueprint_replication_condition: unreal_asset::enums::ELifetimeCondition::CondNone,
            serialized_type: Some(asset.add_fname("ObjectProperty")),
        },
        property_class: actor,
    };

    let (fi, func) = asset
        .asset_data
        .exports
        .iter_mut()
        .enumerate()
        .find_map(|(i, e)| {
            if let unreal_asset::exports::Export::FunctionExport(func) = e {
                if func
                    .get_base_export()
                    .object_name
                    .get_content(|n| n == "ReceiveBeginPlay")
                {
                    return Some((PackageIndex::from_export(i as i32).unwrap(), func));
                }
            }
            None
        })
        .context("ReceiveBeginPlay function not found in PCB asset")?;

    func.struct_export.loaded_properties.push(prop_class.into());
    func.struct_export
        .loaded_properties
        .push(prop_transform.into());
    func.struct_export
        .loaded_properties
        .push(prop_begin_spawn.into());
    let inst = func
        .struct_export
        .script_bytecode
        .as_mut()
        .context("No script bytecode in ReceiveBeginPlay")?;
    inst.insert(
        0,
        ExLetObj {
            token: EExprToken::ExLetObj,
            variable_expression: Box::new(
                ExLocalVariable {
                    token: EExprToken::ExLocalVariable,
                    variable: KismetPropertyPointer {
                        old: None,
                        new: Some(FieldPath {
                            path: vec![prop_class_name.clone()],
                            resolved_owner: fi,
                        }),
                    },
                }
                .into(),
            ),
            assignment_expression: Box::new(
                ExCallMath {
                    token: EExprToken::ExCallMath,
                    stack_node: load_class,
                    parameters: vec![ExSoftObjectConst {
                        token: EExprToken::ExSoftObjectConst,
                        value: Box::new(
                            ExStringConst {
                                token: EExprToken::ExStringConst,
                                value: MINT_PATH.to_string(),
                            }
                            .into(),
                        ),
                    }
                    .into()],
                }
                .into(),
            ),
        }
        .into(),
    );
    inst.insert(
        1,
        ExLet {
            token: EExprToken::ExLet,
            value: KismetPropertyPointer {
                old: None,
                new: Some(FieldPath {
                    path: vec![prop_transform_name.clone()],
                    resolved_owner: fi,
                }),
            },
            variable: Box::new(
                ExLocalVariable {
                    token: EExprToken::ExLocalVariable,
                    variable: KismetPropertyPointer {
                        old: None,
                        new: Some(FieldPath {
                            path: vec![prop_transform_name.clone()],
                            resolved_owner: fi,
                        }),
                    },
                }
                .into(),
            ),
            expression: Box::new(ex_transform.into()),
        }
        .into(),
    );

    inst.insert(
        2,
        ExLetObj {
            token: EExprToken::ExLetObj,
            variable_expression: Box::new(
                ExLocalVariable {
                    token: EExprToken::ExLocalVariable,
                    variable: KismetPropertyPointer {
                        old: None,
                        new: Some(FieldPath {
                            path: vec![prop_begin_spawn_name.clone()],
                            resolved_owner: fi,
                        }),
                    },
                }
                .into(),
            ),
            assignment_expression: Box::new(
                ExCallMath {
                    token: EExprToken::ExCallMath,
                    stack_node: begin_spawning,
                    parameters: vec![
                        ExSelf {
                            token: EExprToken::ExSelf,
                        }
                        .into(),
                        ExLocalVariable {
                            token: EExprToken::ExLocalVariable,
                            variable: KismetPropertyPointer {
                                old: None,
                                new: Some(FieldPath {
                                    path: vec![prop_class_name],
                                    resolved_owner: fi,
                                }),
                            },
                        }
                        .into(),
                        ExLocalVariable {
                            token: EExprToken::ExLocalVariable,
                            variable: KismetPropertyPointer {
                                old: None,
                                new: Some(FieldPath {
                                    path: vec![prop_transform_name.clone()],
                                    resolved_owner: fi,
                                }),
                            },
                        }
                        .into(),
                        ExByteConst {
                            token: EExprToken::ExByteConst,
                            value: 1,
                        }
                        .into(),
                        ExSelf {
                            token: EExprToken::ExSelf,
                        }
                        .into(),
                    ],
                }
                .into(),
            ),
        }
        .into(),
    );

    inst.insert(
        3,
        ExCallMath {
            token: EExprToken::ExCallMath,
            stack_node: finish_spawning,
            parameters: vec![
                ExLocalVariable {
                    token: EExprToken::ExLocalVariable,
                    variable: KismetPropertyPointer {
                        old: None,
                        new: Some(FieldPath {
                            path: vec![prop_begin_spawn_name],
                            resolved_owner: fi,
                        }),
                    },
                }
                .into(),
                ExLocalVariable {
                    token: EExprToken::ExLocalVariable,
                    variable: KismetPropertyPointer {
                        old: None,
                        new: Some(FieldPath {
                            path: vec![prop_transform_name],
                            resolved_owner: fi,
                        }),
                    },
                }
                .into(),
            ],
        }
        .into(),
    );

    Ok(())
}

fn patch_is_modded(
    is_modded: Option<PackageIndex>,
    is_modded_sandbox: Option<PackageIndex>,
    mut statement: TrackedStatement,
) -> Option<TrackedStatement> {
    walk(&mut statement.ex, &|ex| {
        if let KismetExpression::ExCallMath(f) = ex {
            if Some(f.stack_node) == is_modded || Some(f.stack_node) == is_modded_sandbox {
                *ex = ExFalse::default().into()
            }
        }
    });
    Some(statement)
}

pub fn patch_sandbox<C: Seek + Read>(asset: &mut Asset<C>) -> Result<()> {
    let ver = AssetVersion::new_from(asset);
    let mut statements = extract_tracked_statements(asset, ver, &None);

    let find_function = |name| {
        asset
            .imports
            .iter()
            .enumerate()
            .find(|(_, i)| {
                i.class_package.get_content(|s| s == "/Script/CoreUObject")
                    && i.class_name.get_content(|s| s == "Function")
                    && i.object_name.get_content(|s| s == name)
            })
            .map(|(pi, _)| PackageIndex::from_import(pi as i32).unwrap())
    };

    let is_modded = find_function("FSDIsModdedServer");
    let is_modded_sandbox = find_function("FSDIsModdedSandboxServer");

    for statements in statements.values_mut() {
        *statements = std::mem::take(statements)
            .into_iter()
            .filter_map(|s| patch_is_modded(is_modded, is_modded_sandbox, s))
            .collect();
    }
    inject_tracked_statements(asset, ver, statements);
    Ok(())
}

#[allow(dead_code)]
pub fn patch_modding_tab<C: Seek + Read>(asset: &mut Asset<C>) -> Result<()> {
    let ver = AssetVersion::new_from(asset);
    let mut statements = extract_tracked_statements(asset, ver, &None);

    for statements in statements.values_mut() {
        for statement in statements {
            walk(&mut statement.ex, &|ex| {
                if let KismetExpression::ExSetArray(arr) = ex {
                    if arr.elements.len() == 2 {
                        arr.elements.retain(|e| !matches!(e, KismetExpression::ExInstanceVariable(v) if v.variable.new.as_ref().unwrap().path.last().unwrap().get_content(|c| c == "BTN_Modding")));
                        if arr.elements.len() != 2 {
                            info!("patched modding tab visibility");
                        }
                    }
                }
            });
        }
    }
    inject_tracked_statements(asset, ver, statements);
    Ok(())
}

#[allow(dead_code)]
pub fn patch_modding_tab_item<C: Seek + Read>(asset: &mut Asset<C>) -> Result<()> {
    let itm_tab_modding = get_import(
        asset,
        vec![
            Import::new(
                "/Script/CoreUObject",
                "Package",
                "/Game/UI/Menu_EscapeMenu/Modding/ITM_Tab_Modding",
            ),
            Import::new(
                "/Script/UMG",
                "WidgetBlueprintGeneratedClass",
                "ITM_Tab_Modding_C",
            ),
        ],
    );
    let itm_tab_modding_cdo = get_import(
        asset,
        vec![
            Import::new(
                "/Script/CoreUObject",
                "Package",
                "/Game/UI/Menu_EscapeMenu/Modding/ITM_Tab_Modding",
            ),
            Import::new(
                "/Game/UI/Menu_EscapeMenu/Modding/ITM_Tab_Modding",
                "ITM_Tab_Modding_C",
                "Default__ITM_Tab_Modding_C",
            ),
        ],
    );

    let new_class = asset.add_fname("MI_UI_C");
    let new_cdo = asset.add_fname("Default__MI_UI_C");
    let new_package = asset.add_fname("/Game/_AssemblyStorm/ModIntegration/MI_UI");

    // TODO add get_import_mut or something so indexes don't have to be handled manually

    asset.imports[(-itm_tab_modding_cdo.index - 1) as usize].object_name = new_cdo;
    asset.imports[(-itm_tab_modding_cdo.index - 1) as usize].class_package = new_package.clone();
    asset.imports[(-itm_tab_modding_cdo.index - 1) as usize].class_name = new_class.clone();

    let package_index = {
        let obj = &mut asset.imports[(-itm_tab_modding.index - 1) as usize];
        obj.object_name = new_class;
        obj.outer_index
    };

    asset.imports[(-package_index.index - 1) as usize].object_name = new_package;

    Ok(())
}

pub fn patch_server_list_entry<C: Seek + Read>(asset: &mut Asset<C>) -> Result<()> {
    let get_mods_installed = asset
        .imports
        .iter()
        .enumerate()
        .find(|(_, i)| {
            i.class_package.get_content(|s| s == "/Script/CoreUObject")
                && i.class_name.get_content(|s| s == "Function")
                && i.object_name.get_content(|s| s == "FSDGetModsInstalled")
        })
        .map(|(pi, _)| PackageIndex::from_import(pi as i32).unwrap());

    let fsd_target_platform = asset
        .imports
        .iter()
        .enumerate()
        .find(|(_, i)| {
            i.class_package.get_content(|s| s == "/Script/CoreUObject")
                && i.class_name.get_content(|s| s == "Function")
                && i.object_name.get_content(|s| s == "FSDTargetPlatform")
        })
        .map(|(pi, _)| PackageIndex::from_import(pi as i32).unwrap());

    let ver = AssetVersion::new_from(asset);
    let mut statements = extract_tracked_statements(asset, ver, &None);

    for (pi, statements) in statements.iter_mut() {
        let name = &asset
            .asset_data
            .get_export(*pi)
            .unwrap()
            .get_base_export()
            .object_name;

        let swap_platform = name.get_content(|c| ["GetMissionToolTip", "SetSession"].contains(&c));

        for statement in statements {
            walk(&mut statement.ex, &|ex| {
                if let KismetExpression::ExCallMath(cm) = ex {
                    if Some(cm.stack_node) == get_mods_installed && cm.parameters.len() == 2 {
                        cm.parameters[1] = ExFalse {
                            token: EExprToken::ExFalse,
                        }
                        .into();
                        info!("patched server list entry");
                    }
                    if swap_platform && Some(cm.stack_node) == fsd_target_platform {
                        *ex = ExByteConst {
                            token: EExprToken::ExByteConst,
                            value: 0,
                        }
                        .into();
                    }
                }
            });
        }
    }
    inject_tracked_statements(asset, ver, statements);

    {
        // swap out tooltip with rebuilt version
        let itm_tab_modding = get_import(
            asset,
            vec![
                Import::new(
                    "/Script/CoreUObject",
                    "Package",
                    "/Game/UI/Menu_ServerList/TOOLTIP_ServerEntry_Mods",
                ),
                Import::new(
                    "/Script/UMG",
                    "WidgetBlueprintGeneratedClass",
                    "TOOLTIP_ServerEntry_Mods_C",
                ),
            ],
        );
        let itm_tab_modding_cdo = get_import(
            asset,
            vec![
                Import::new(
                    "/Script/CoreUObject",
                    "Package",
                    "/Game/UI/Menu_ServerList/TOOLTIP_ServerEntry_Mods",
                ),
                Import::new(
                    "/Game/UI/Menu_ServerList/TOOLTIP_ServerEntry_Mods",
                    "TOOLTIP_ServerEntry_Mods_C",
                    "Default__TOOLTIP_ServerEntry_Mods_C",
                ),
            ],
        );
        let new_package = asset.add_fname(
            "/Game/_AssemblyStorm/ModIntegration/RebuiltAssets/TOOLTIP_ServerEntry_Mods",
        );
        asset.imports[(-itm_tab_modding_cdo.index - 1) as usize].class_package =
            new_package.clone();
        let package_index = {
            let obj = &mut asset.imports[(-itm_tab_modding.index - 1) as usize];
            obj.outer_index
        };
        asset.imports[(-package_index.index - 1) as usize].object_name = new_package;
    }

    Ok(())
}

pub fn patch_init_actors<C: Read + Seek>(
    asset: &mut Asset<C>,
    init_space_rig: HashSet<String>,
    init_cave: HashSet<String>,
) {
    let init_spacerig_fnames = init_space_rig
        .into_iter()
        .map(|p| asset.add_fname(&p))
        .collect::<Vec<_>>();
    let init_cave_fnames = init_cave
        .into_iter()
        .map(|p| asset.add_fname(&p))
        .collect::<Vec<_>>();

    let ancestry = Ancestry::new(FName::new_dummy("".to_owned(), 0));

    if let Some(e) = find_export_named(asset, "Default__MI_SpawnMods_C") {
        if let Some((_, p)) = find_array_property_named(e, "SpaceRigMods") {
            p.value.clear();
            for path in init_spacerig_fnames {
                p.value.push(
                    SoftObjectProperty {
                        name: FName::new_dummy("0".to_owned(), -2147483648),
                        ancestry: ancestry.clone(),
                        property_guid: None,
                        duplication_index: 0,
                        value: SoftObjectPath {
                            asset_path: TopLevelAssetPath::new(None, path),
                            sub_path_string: None,
                        },
                    }
                    .into(),
                );
            }
        }
        if let Some((_, p)) = find_array_property_named(e, "CaveMods") {
            p.value.clear();
            for path in init_cave_fnames {
                p.value.push(
                    SoftObjectProperty {
                        name: FName::new_dummy("0".to_owned(), -2147483648),
                        ancestry: ancestry.clone(),
                        property_guid: None,
                        duplication_index: 0,
                        value: SoftObjectPath {
                            asset_path: TopLevelAssetPath::new(None, path),
                            sub_path_string: None,
                        },
                    }
                    .into(),
                );
            }
        }
    }
}
