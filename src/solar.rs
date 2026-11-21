use core_maths::CoreFloat;

pub const BUENOS_AIRES_LATITUDE: f64 = -34.6037;
pub const BUENOS_AIRES_LONGITUDE: f64 = -58.3816;

const SUNRISE_ZENITH_DEGREES: f64 = 90.833;
const CIVIL_TWILIGHT_ZENITH_DEGREES: f64 = 96.0;

pub struct SunTimes {
    pub civil_dawn: f64,
    pub sunrise: f64,
    pub sunset: f64,
    pub civil_dusk: f64,
}

pub fn for_date(year: i32, month: u32, day: u32) -> SunTimes {
    let solar_parameters = SolarParameters::at(
        crate::civil::day_of_year(year, month, day),
        BUENOS_AIRES_LATITUDE,
        BUENOS_AIRES_LONGITUDE,
    );

    SunTimes {
        civil_dawn: solar_parameters.local_milliseconds(CIVIL_TWILIGHT_ZENITH_DEGREES, true),
        sunrise: solar_parameters.local_milliseconds(SUNRISE_ZENITH_DEGREES, true),
        sunset: solar_parameters.local_milliseconds(SUNRISE_ZENITH_DEGREES, false),
        civil_dusk: solar_parameters.local_milliseconds(CIVIL_TWILIGHT_ZENITH_DEGREES, false),
    }
}

struct SolarParameters {
    declination: f64,
    equation_of_time_minutes: f64,
    latitude: f64,
    longitude: f64,
}

impl SolarParameters {
    fn at(day_of_year: u32, latitude: f64, longitude: f64) -> Self {
        let fractional_year =
            2.0 * core::f64::consts::PI / 365.0 * (day_of_year as f64 - 1.0 + 0.5);

        let declination = 0.006918 - 0.399912 * fractional_year.cos()
            + 0.070257 * fractional_year.sin()
            - 0.006758 * (2.0 * fractional_year).cos()
            + 0.000907 * (2.0 * fractional_year).sin()
            - 0.002697 * (3.0 * fractional_year).cos()
            + 0.00148 * (3.0 * fractional_year).sin();

        let equation_of_time_minutes = 229.18
            * (0.000075 + 0.001868 * fractional_year.cos()
                - 0.032077 * fractional_year.sin()
                - 0.014615 * (2.0 * fractional_year).cos()
                - 0.040849 * (2.0 * fractional_year).sin());

        Self {
            declination,
            equation_of_time_minutes,
            latitude: latitude.to_radians(),
            longitude,
        }
    }

    fn local_milliseconds(&self, zenith_degrees: f64, rising: bool) -> f64 {
        let hour_angle_degrees = self.hour_angle_degrees(zenith_degrees);
        let direction = if rising { 1.0 } else { -1.0 };

        let utc_minutes = 720.0
            - 4.0 * (self.longitude + direction * hour_angle_degrees)
            - self.equation_of_time_minutes;

        let local_seconds = utc_minutes * 60.0 + crate::civil::BUENOS_AIRES_OFFSET_SECONDS as f64;
        let milliseconds_into_day = local_seconds * 1000.0;

        milliseconds_into_day.rem_euclid(86_400_000.0)
    }

    fn hour_angle_degrees(&self, zenith_degrees: f64) -> f64 {
        let zenith = zenith_degrees.to_radians();
        let numerator = zenith.cos() - self.latitude.sin() * self.declination.sin();
        let denominator = self.latitude.cos() * self.declination.cos();

        (numerator / denominator).clamp(-1.0, 1.0).acos().to_degrees()
    }
}
