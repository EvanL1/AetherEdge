-- AetherEMS Energy Pack migration/commissioning preset.
-- The Aether Kernel and Pack activation never execute this file automatically.
INSERT INTO calculated_points (id, name, formula, unit, imgurl, description, created_at, updated_at) VALUES
(1, 'PV Energy', '', 'kWh', 'icon-pv-energy', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(2, 'Diesel Energy', '', 'KWh', 'icon-diesel-energy', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(3, 'Energy Used', '', 'kWh', 'icon-energy-used', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(4, 'Saving Billing', '', '$', 'icon-saving-billing', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(5, 'PV', '', 'kW', 'icon-pv-energy', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(6, 'Diesel', '', 'kW', 'icon-diesel-energy', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(7, 'ESS', '', 'KWh', 'icon-ess-energy', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(8, 'P', '', 'KW', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(9, 'U', '', 'V', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(10, 'P', '', 'KW', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(11, 'U', '', 'V', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(12, 'P', '', 'KW', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(13, 'U', '', 'V', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(14, 'P', '', 'kw', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(15, 'P', '', 'kw', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(16, 'P', '', 'kw', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(17, 'Oil', '', '%', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(18, 'P', '', 'kw', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
(19, 'SOC', '', '%', '', '', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
