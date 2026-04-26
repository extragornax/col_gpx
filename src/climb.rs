/// A detected climb within a single GPX track.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DetectedClimb {
    pub start_km: f64,
    pub end_km: f64,
    pub start_ele: f64,
    pub end_ele: f64,
    pub gain: f64,
    pub gradient: f64,
    pub lat: f64,
    pub lon: f64,
}

/// A profile point: (distance_km, elevation_m, lat, lon).
pub type ProfilePoint = (f64, f64, f64, f64);

/// Detect climbs from a resampled elevation profile.
/// `min_gain`: minimum elevation gain in meters (e.g. 50).
/// Climbs below 1% average gradient are filtered out (river valleys, not real climbs).
pub fn detect_climbs(profile: &[ProfilePoint], min_gain: f64) -> Vec<DetectedClimb> {
    if profile.len() < 2 {
        return Vec::new();
    }

    let pts = resample(profile, 0.2);
    if pts.len() < 2 {
        return Vec::new();
    }

    let end_drop = 15.0;
    let flat_dist = 1.0;

    let mut climbs = Vec::new();
    let mut low_km = pts[0].0;
    let mut low_ele = pts[0].1;
    let mut high_km = pts[0].0;
    let mut high_ele = pts[0].1;
    let mut in_climb = false;
    let mut summit_lat = pts[0].2;
    let mut summit_lon = pts[0].3;
    let mut start_ele = pts[0].1;

    for &(km, ele, lat, lon) in &pts[1..] {
        if !in_climb {
            if ele < low_ele {
                low_km = km;
                low_ele = ele;
                start_ele = ele;
                high_km = km;
                high_ele = ele;
            }
            if ele > high_ele {
                high_km = km;
                high_ele = ele;
                summit_lat = lat;
                summit_lon = lon;
            }
            if high_ele - low_ele >= min_gain {
                in_climb = true;
            }
        } else {
            if ele > high_ele {
                high_km = km;
                high_ele = ele;
                summit_lat = lat;
                summit_lon = lon;
            }
            let dropped = high_ele - ele >= end_drop;
            let flat = km - high_km >= flat_dist;
            if dropped || flat {
                let gain = high_ele - low_ele;
                if gain >= min_gain {
                    let dist = high_km - low_km;
                    let gradient = if dist > 0.001 { gain / (dist * 10.0) } else { 0.0 };
                    climbs.push(DetectedClimb {
                        start_km: low_km,
                        end_km: high_km,
                        start_ele,
                        end_ele: high_ele,
                        gain,
                        gradient,
                        lat: summit_lat,
                        lon: summit_lon,
                    });
                }
                in_climb = false;
                low_km = km;
                low_ele = ele;
                start_ele = ele;
                high_km = km;
                high_ele = ele;
                summit_lat = lat;
                summit_lon = lon;
            }
        }
    }

    if in_climb {
        let gain = high_ele - low_ele;
        if gain >= min_gain {
            let dist = high_km - low_km;
            let gradient = if dist > 0.001 { gain / (dist * 10.0) } else { 0.0 };
            climbs.push(DetectedClimb {
                start_km: low_km,
                end_km: high_km,
                start_ele,
                end_ele: high_ele,
                gain,
                gradient,
                lat: summit_lat,
                lon: summit_lon,
            });
        }
    }

    climbs.retain(|c| c.gradient >= 1.0);
    climbs
}

pub struct GpxProfile {
    pub points: Vec<ProfilePoint>,
    pub date: Option<String>,
}

/// Build a profile from GPX XML, also extracting the activity date from metadata or the first trackpoint.
pub fn profile_from_gpx(xml: &[u8]) -> anyhow::Result<GpxProfile> {
    let gpx = gpx::read(xml)?;
    let mut points = Vec::new();
    let mut total_dist = 0.0_f64;
    let mut prev: Option<(f64, f64)> = None;
    let mut first_time: Option<String> = None;

    if let Some(ref meta) = gpx.metadata
        && let Some(ref t) = meta.time
        && let Ok(s) = t.format()
    {
        first_time = Some(s[..10].to_string());
    }

    for track in &gpx.tracks {
        for segment in &track.segments {
            for pt in &segment.points {
                let lat = pt.point().y();
                let lon = pt.point().x();
                let ele = pt.elevation.unwrap_or(0.0);

                if first_time.is_none()
                    && let Some(ref t) = pt.time
                    && let Ok(s) = t.format()
                {
                    first_time = Some(s[..10].to_string());
                }

                if let Some((plat, plon)) = prev {
                    total_dist += haversine_km(plat, plon, lat, lon);
                }
                prev = Some((lat, lon));
                points.push((total_dist, ele, lat, lon));
            }
        }
    }
    Ok(GpxProfile { points, date: first_time })
}

fn resample(profile: &[ProfilePoint], step_km: f64) -> Vec<ProfilePoint> {
    if profile.is_empty() {
        return Vec::new();
    }
    let total_km = profile.last().unwrap().0;
    let n = ((total_km / step_km).ceil() as usize).max(1);
    let mut out = Vec::with_capacity(n + 1);
    let mut j = 0;

    for i in 0..=n {
        let target_km = step_km * i as f64;
        while j + 1 < profile.len() && profile[j + 1].0 < target_km {
            j += 1;
        }
        if j + 1 >= profile.len() {
            out.push(*profile.last().unwrap());
            break;
        }
        let (km0, ele0, lat0, lon0) = profile[j];
        let (km1, ele1, lat1, lon1) = profile[j + 1];
        let t = if (km1 - km0).abs() > 1e-9 {
            (target_km - km0) / (km1 - km0)
        } else {
            0.0
        };
        out.push((
            target_km,
            ele0 + t * (ele1 - ele0),
            lat0 + t * (lat1 - lat0),
            lon0 + t * (lon1 - lon0),
        ));
    }
    out
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    r * 2.0 * a.sqrt().asin()
}
