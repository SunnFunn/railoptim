from pathlib import Path

import pandas as pd
import pyodbc

# servers and DBs
# /*ИСУ ПВ*/MSKOM1.OptimizerV2
# /*ИСУ ПВ*/MSKOM1.Dprognoz
# /*АСУ ВП*/MSKASUVPL.ASUVP_RAT
# /*RailTariff*/MSKASUVPL.RAT_RailTariff
# /*РАТ Онлайн*/MSKASUVPL.SLP

DRIVER = "{ODBC Driver 18 for SQL Server}"
SERVER = "MSKASUVPL"
DATABASE = "SLP"

ROOT = Path(__file__).resolve().parent
SQL_PATH = ROOT / "gu12.sql"


def fetch_GU12(
    path=ROOT,
    driver=DRIVER,
    server=SERVER,
    database=DATABASE,
):
    connection_string = (
        "Trusted_Connection=yes;"
        f"Driver={driver};"
        f"Server={server};"
        f"Database={database};"
        "TrustServerCertificate=yes;"
    )

    stmt = SQL_PATH.read_text(encoding="utf-8")
    out_dir = Path(path)
    out_dir.mkdir(parents=True, exist_ok=True)

    conn = pyodbc.connect(connection_string)
    try:
        data = pd.read_sql(stmt, conn)
    finally:
        conn.close()

    excel_path = out_dir / "GU12.xlsx"
    json_path = out_dir / "GU12.json"

    data.to_excel(excel_path, index=False)
    data.to_json(
        json_path,
        orient="records",
        force_ascii=False,
        date_format="iso",
        indent=2,
    )

    print(f"Строк: {len(data)}")
    print(f"Excel: {excel_path}")
    print(f"JSON:  {json_path}")
    return data


if __name__ == "__main__":
    fetch_GU12()
