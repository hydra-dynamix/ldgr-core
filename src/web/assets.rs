const INDEX_HTML: &str = include_str!("../web_assets/index.html");

const APP_CSS: &str = include_str!("../web_assets/app.css");

const APP_JS: &str = concat!(
    include_str!("../web_assets/app/state_api.js"),
    "\n",
    include_str!("../web_assets/app/render_core.js"),
    "\n",
    include_str!("../web_assets/app/conduct_controls.js"),
    "\n",
    include_str!("../web_assets/app/utils_boot.js"),
);

