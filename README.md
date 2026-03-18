# MOVE Data M2 — CD Release ETL Guide

## Overview

This folder contains the tooling to ETL Continuous Delivery (CD) releases from Ipsos into our format and partitioning structure, then make them available in the MOVE system.

**Data flow:**
1. Ipsos delivers a new CD release as parquet files in S3 (`s3://ipsosdatarepo/CD/CCF/<release>/`), split across four site types: `indoor/`, `placebased/`, `roadside/`, `transit/`
2. We run the EMR job to combine, transform, and re-partition the data into our optimised format
3. We create an Athena table over the combined output, then a view over that table
4. We register the release in the app database so it appears in the UI

## Files

| File | Purpose |
|------|---------|
| `setup_emr_iam.sh` | **Run once.** Creates IAM roles, instance profiles, EC2 key pair |
| `emr_combine_etl.py` | PySpark ETL script — the actual Spark job |
| `run_emr_job.sh` | Uploads script to S3 and launches the EMR cluster |
| `athena_create_table.sql` | Athena DDL to create the combined table + partitions after the job |
| `facepid_bucket.py` | Utility to compute `facepid_bucket` values matching Spark's hash, for building WHERE clauses |

---

## New CD Release Process

> **Note:** This process may vary per release. CD8 was a full replacement release so no layering of prior CDs was needed. The steps below describe the standard process.

### Step 1 — Receive the release

You will be notified of the new CD with the S3 path to the source data, e.g.:

```
s3://ipsosdatarepo/CD/CCF/CD9_15052026/
```

### Step 2 — Configure the ETL

Update `emr_combine_etl.py` for the new release:

- Set the source paths for each site type (`indoor/`, `placebased/`, `roadside/`, `transit/`)
- Set `S3_OUTPUT` to the combined output path (e.g. `.../CD9_15052026/combined/`)
- Set `RELEASE` to the release identifier (e.g. `cd9_15052026`) — this becomes the `release` column value in the output

### Step 3 — Run the EMR job

```bash
chmod +x run_emr_job.sh
./run_emr_job.sh
```

See [EMR Job Setup](#emr-job-setup) below for prerequisites and monitoring.

### Step 4 — Create the Athena table

After the EMR job completes, run `athena_create_table.sql` in the Athena console (updated for the new release):

1. `CREATE EXTERNAL TABLE` — defines the schema over the combined output location
2. `MSCK REPAIR TABLE` — discovers all partitions
3. Verification query — spot-check row counts by type

### Step 5 — Create the master view

Create a view over the combined table. Views are used because data sometimes needs to be adjusted or supplemented after the initial release — the view layer allows this without touching the underlying data.

We maintain two prefixes mapping to our two application environments:
- `stg_` — staging environment (used to verify changes safely)
- `prd_` — production environment (only promoted to once verified)

Always create the staging view first:

```sql
CREATE OR REPLACE VIEW stg_cd9_15052026_master AS
SELECT * FROM cd9_15052026_combined;
```

Once verified in staging, create the production view:

```sql
CREATE OR REPLACE VIEW prd_cd9_15052026_master AS
SELECT * FROM cd9_15052026_combined;
```

### Step 6 — Register the release in the app database

In `public.data_release`, add a new row for the release. Use the previous release row as a template.

**The `dates` field controls ordering in the UI dropdown** — earlier date = higher in the list, so new releases should be at the top. Use the month prior to the previous CD's release date.

```sql
INSERT INTO public.data_release (name, type, dates, ...)
VALUES ('CD9 — May 2026', 'master', '2026-01-01', ...);
```

---

## EMR Job Setup

### Prerequisites

- AWS CLI configured with appropriate credentials
- IAM roles created (see Step 1 below)
- `SUBNET_ID` set in `run_emr_job.sh`

### Step 1: IAM Setup (one-time)

```bash
chmod +x setup_emr_iam.sh
./setup_emr_iam.sh ap-southeast-2
```

This creates:
- `EMR_DefaultRole` — EMR service role
- `EMR_EC2_DefaultRole` — EC2 instance role + instance profile
- Scoped S3 access policy for `ipsosdatarepo`
- EC2 key pair `emr-cd8-keypair`

### Step 2: Find Your Subnet ID

```bash
aws ec2 describe-subnets --region ap-southeast-2 \
  --filters 'Name=default-for-az,Values=true' \
  --query 'Subnets[0].SubnetId' --output text
```

### Step 3: Edit Configuration

Open `run_emr_job.sh` and update:

```bash
REGION="ap-southeast-2"
SUBNET_ID="subnet-xxxxxxxxxxxxxxxxx"   # From step 2
```

### Step 4: Launch

```bash
./run_emr_job.sh
```

### Step 5: Monitor

```bash
# Cluster status
aws emr describe-cluster --cluster-id j-XXXXXXXXXXXXX --region ap-southeast-2 --query 'Cluster.Status'

# Step progress
aws emr list-steps --cluster-id j-XXXXXXXXXXXXX --region ap-southeast-2

# SSH for Spark UI
aws emr ssh --cluster-id j-XXXXXXXXXXXXX --key-pair-file emr-cd8-keypair.pem --region ap-southeast-2
# Then open: http://localhost:18080
```

Logs: `s3://ipsosdatarepo/emr-logs/<release>/<cluster-id>/`

---

## Architecture

### Output Format
**Parquet + Snappy** — Athena's optimal format. Columnar layout + lightweight compression gives the best scan speed and cost.

### Partitioning: `year/month/facepid_bucket`
- **year/month** — Extracted from `contact_datetime`. Most queries filter by time range, so this gives Athena partition pruning for free.
- **facepid_bucket** — ~50k distinct facepids hashed into **64 buckets**. At 25TB, this keeps individual partition files in the 128–512MB Athena sweet spot.

### Sort Order: `facepid, contact_datetime, respid`
Data is sorted within each partition by these columns since they are the primary query filters. This improves Parquet row-group min/max statistics so Athena can skip irrelevant row groups.

### Type Harmonisation
Each site type has schema differences normalised during the ETL:

| Source | Issue | Fix |
|--------|-------|-----|
| placebased | `contact_time` column doesn't exist | Hardcoded to `0.0` |
| placebased | `freq` is DOUBLE | Cast to BIGINT |
| roadside | `rots` is DOUBLE | Cast to BIGINT |
| roadside | `freq` is DOUBLE | Cast to BIGINT |
| transit | `contact_time` is INT64 | Cast to DOUBLE |

All sources are aligned to: `facepid/respid/rots/freq/va` as BIGINT; `person_weight/va_prob/contact_time/random_value` as DOUBLE.

### Cluster Sizing (CD8 — 25.8TB)

| Component | Instance | Count | Resources |
|-----------|----------|-------|-----------|
| Primary (driver) | m5.4xlarge | 1 | 16 vCPU, 64GB RAM |
| Core (workers) | i3.4xlarge | 20 | 16 vCPU, 122GB RAM, 2×1.9TB NVMe SSD |
| **Total** | | **21** | **336 vCPU, 2.5TB RAM, 76TB local SSD** |

**Why i3.4xlarge?** At 25TB, the shuffle/repartition step spills significant data to disk. EBS-backed instances would bottleneck on I/O — the i3's local NVMe storage provides the throughput needed. Adjust `CORE_INSTANCE_COUNT` in `run_emr_job.sh` proportionally for larger or smaller releases.

**Cost estimate:** ~$60–120 (20× i3.4xlarge @ ~$1.25/hr × 2–4 hours) + ~$5–10 S3 requests.

### Troubleshooting

| Symptom | Likely Cause | Fix |
|---------|-------------|-----|
| Cluster fails to start | Missing IAM roles | Re-run `setup_emr_iam.sh` |
| "Access Denied" on S3 | Instance profile missing S3 policy | Check `EMR_EC2_DefaultRole` has the S3 policy |
| Step fails with OOM | Executor memory too low | Increase `CORE_INSTANCE_COUNT` or use i3.8xlarge |
| Job takes >6 hours | Too few nodes or shuffle spill to EBS | Increase `CORE_INSTANCE_COUNT` to 30 |
| Many small output files | Not enough data per partition | Reduce `NUM_BUCKETS` from 64 to 32 |

---

## Utilities

### `facepid_bucket.py`

When querying the combined table by facepid, include `facepid_bucket` in your WHERE clause to use partition pruning. Spark's hash function and Athena's differ, so you can't compute the bucket inline in SQL — use this script instead.

Populate the `FACEPIDS` list and run:

```bash
python facepid_bucket.py
```

Output includes the bucket assignments and a ready-to-use `AND facepid_bucket IN (...)` clause for your query.
