use ply_kernel::PluginSource;

/// Replace the host model import in both describe and complete modules.
pub fn mock_model(plugin: PluginSource, fixture: &str) -> PluginSource {
    match plugin {
        PluginSource::Bundled { name, mut files } if name == "agent" => {
            let import = "import { request as modelRequest } from \"./pi\";";
            let mut replaced = 0;
            for (_, source) in &mut files {
                if source.contains(import) {
                    *source = source.replacen(import, fixture, 1);
                    replaced += 1;
                }
            }
            assert_eq!(replaced, 2, "fixture must cover describe and complete");
            PluginSource::Bundled { name, files }
        }
        other => other,
    }
}
