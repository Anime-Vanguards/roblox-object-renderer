// Terrain Clouds as this build draws them (render::clouds, docs/rendering.md "Clouds"): a generic
// single-scattering ray march, not calibrated against Studio. The layer is a shell round a planet the
// camera stands on, so it bends down to the horizon; its shape is the cloud volume of the user's Studio
// install (`content/sky/clouds.dds`, tiled across the layer, its slices up through it) with the detail
// volume (`cloudDetail3D.dds`) eating into its edges, cloud where the field falls under Cover's
// threshold. Light: Beer-Lambert extinction; the sun through a two-lobe Henyey-Greenstein phase
// function, dimmed by a short march towards it and kept from going black by a share standing in for
// multiple scattering; the sky's light from above and the flat ambient. The sky pass (sky.wgsl) calls
// `clouds` and `over_clouds`.

// The layer, in units of CLOUD_STUDS studs: a planet of radius CLOUD_PLANET under the camera and the
// shell from CLOUD_BOTTOM to CLOUD_TOP round its centre (6,000 to 16,000 studs above the camera)
const CLOUD_STUDS: f32 = 4000.0;
const CLOUD_PLANET: f32 = 1000.0;
const CLOUD_BOTTOM: f32 = 1001.5;
const CLOUD_TOP: f32 = 1004.0;
// The shape volume tiles this many units across
const CLOUD_TILE: f32 = 32.0;
// The longest stretch of a ray marched through the layer, in layer thicknesses: towards the horizon
// the path through the shell grows long, and what lies past this is too far to tell apart
const CLOUD_REACH: f32 = 6.0;
const CLOUD_LIGHT_STEPS: i32 = 3;
const CLOUD_PI: f32 = 3.14159265;

// Henyey-Greenstein's phase function: the share of light scattered at an angle whose cosine is
// `cosine` (1: on along the light), for an asymmetry `g`; it integrates to 1 over the sphere
fn cloud_hg(cosine: f32, g: f32) -> f32 {
	let g2 = g * g;
	return (1.0 - g2) / (4.0 * CLOUD_PI * pow(max(1.0 + g2 - 2.0 * g * cosine, 1e-4), 1.5));
}

// by the cosine from the view to the sun: a strong forward lobe (bright edges looking towards the
// sun) and a weak backward one
fn cloud_phase(towards: f32) -> f32 {
	return 0.8 * cloud_hg(towards, 0.6) + 0.2 * cloud_hg(towards, -0.3);
}

// The shape field at `p` (x and z across in units, y the height through the layer, 0 to 1) with the
// detail volume eating into it, at a mip level: cloud where it falls under the threshold
fn cloud_field(p: vec3<f32>, level: f32) -> f32 {
	let detail = textureSampleLevel(cloud_detail, cloud_sampler, p * sky.cloud_shape_params.x, level).r;
	let shape = textureSampleLevel(cloud_shape, cloud_sampler, vec3<f32>(p.x / CLOUD_TILE, p.z / CLOUD_TILE, p.y), level).r;
	return shape + sky.cloud_color.w * (detail * 2.0 - 1.0);
}

// A point on the planet's frame (its centre at the origin) in the layer's space
fn cloud_space(point: vec3<f32>) -> vec3<f32> {
	return vec3<f32>(point.x, (length(point) - CLOUD_BOTTOM) / (CLOUD_TOP - CLOUD_BOTTOM), point.z);
}

// What the clouds along `direction` add (rgb, radiance, as much as they cover) and how much they cover (a)
fn clouds(direction: vec3<f32>) -> vec4<f32> {
	if (sky.clouds.x < 0.5 || direction.y <= 0.0) {
		return vec4<f32>(0.0);
	}
	// the camera's place on the planet, and where its ray enters and leaves the shell
	let camera = frame.camera_position.xyz / CLOUD_STUDS;
	let origin = camera + vec3<f32>(0.0, CLOUD_PLANET, 0.0);
	let b = dot(direction, origin);
	let c = dot(origin, origin);
	let enter = max(-b + sqrt(max(b * b - (c - CLOUD_BOTTOM * CLOUD_BOTTOM), 0.0)), 0.0);
	let leave = -b + sqrt(max(b * b - (c - CLOUD_TOP * CLOUD_TOP), 0.0));
	let thickness = CLOUD_TOP - CLOUD_BOTTOM;
	let span = min(leave - enter, CLOUD_REACH * thickness);
	if (span <= 0.0) {
		return vec4<f32>(0.0);
	}
	let count = f32(max(i32(sky.clouds.w), 1));
	let step = span / count;
	let threshold = sky.clouds.y;
	// thinning out as the camera nears the layer
	let below = CLOUD_BOTTOM - CLOUD_PLANET;
	let density = sky.clouds.z * saturate((below - camera.y) / (0.25 * below));
	let extinction = sky.cloud_sky.w;
	let sun = frame.sun_direction.xyz;
	let towards = dot(direction, sun);
	// the sun's light: through the phase function (4π of it: an even scattering lights the cloud as a
	// white surface facing the sun), as strong as Lighting makes it
	let sun_light = frame.sun_color.rgb * sky.cloud_light.x * cloud_phase(towards) * 4.0 * CLOUD_PI;
	// the sky's light from above, darker towards the layer's bottom, never under the flat ambient
	let sky_above = frame.sky_cube[2].rgb * sky.cloud_light.y;
	let flat_ambient = max(frame.ambient.rgb, frame.outdoor_ambient.rgb) * sky.cloud_light.z;
	// towards the sun from a point: up through the rest of the layer, kept at least 0.3 off level
	var sun_up = sun;
	sun_up.y = max(sun.y, 0.3);
	sun_up = normalize(sun_up);
	var light = vec3<f32>(0.0);
	var through = 1.0;
	for (var i = 0.0; i < count; i += 1.0) {
		let t = enter + (i + sky.cloud_shape_params.y) * step;
		let point = origin + direction * t;
		let p = cloud_space(point);
		let level = max(0.0, log2(max(t, 1e-3) * 0.03));
		let field = cloud_field(p, level);
		if (field < threshold) {
			let sigma = density * extinction * 40.0 * (threshold - field);
			// what lies between the point and the sun
			let reach = (1.0 - saturate(p.y)) * thickness / f32(CLOUD_LIGHT_STEPS) / sun_up.y;
			var depth = 0.0;
			for (var j = 0; j < CLOUD_LIGHT_STEPS; j++) {
				let q = cloud_space(point + sun_up * ((f32(j) + 0.5) * reach));
				depth += max(threshold - cloud_field(q, level + 1.0), 0.0);
			}
			let shadow = exp(-density * extinction * 40.0 * depth * reach * sky.cloud_light.w);
			let sunlit = sun_light * mix(shadow, 1.0, sky.cloud_sky.z);
			let skylit = max(sky_above * mix(sky.cloud_sky.x, 1.0, saturate(p.y)), flat_ambient);
			let lit = (sunlit + skylit) * sky.cloud_color.rgb;
			let absorbed = 1.0 - exp(-sigma * step);
			light += through * absorbed * lit;
			through *= 1.0 - absorbed;
			if (through <= 0.003) {
				through = 0.0;
				break;
			}
		}
	}
	return vec4<f32>(light, 1.0 - through);
}

// The clouds over the sky's √ radiance `shown`: their own radiance exposed, clamped to 1 and taken to
// √, laid over it by their cover; nothing where they cover too little
fn over_clouds(shown: vec3<f32>, cloud: vec4<f32>) -> vec3<f32> {
	if (cloud.a <= sky.cloud_shape_params.z) {
		return shown;
	}
	let own = sqrt(clamp(cloud.rgb / cloud.a * sky.params.x, vec3<f32>(0.0), vec3<f32>(1.0)));
	return mix(shown, own, cloud.a);
}
