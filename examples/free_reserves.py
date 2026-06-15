import pandas as pd
import requests

        
def fetch_free_reserves(path = path, driver = driver, server = server, database = database, token=token, url=url):
    header = {'Authorization': 'Bearer {}'.format(token)}

    try:
        response = requests.get(url + "GetFreeReserveCapacityData", headers=header)
        free_reserves_list = response.json()

        # response schema
        # {
        #     "DateReserveCapacity": "2026-06-11T09:23:38.299Z",
        #     "RailWayReserveDivision": "string",
        #     "RailWayReserve": "string",
        #     "RailWayReserveCode": 0,
        #     "StationReserve": "string",
        #     "StationReserveCode": "string",
        #     "ApprovementDocNumber": "string",
        #     "EtranId": 0,
        #     "ReserveOwner": "string",
        #     "ReserveOwnerOKPO": "string",
        #     "AgreementNumber": "string",
        #     "DateBeg": "2026-06-11T09:23:38.299Z",
        #     "DateEnd": "2026-06-11T09:23:38.299Z",
        #     "AgreementReserveCapacity": 0,
        #     "StationReserveCapacity": 0
        # }

        free_reserves_df = pd.DataFrame.from_dict(free_reserves_list)
        free_reserves_df.drop_duplicates(inplace=True)
        free_reserves_df.to_excel("FreeReserves.xlsx")
    except Exception as e:
        print(f"Error: {e}")
        print(response.status_code, response.json())
    finally:
        print(len(free_reserves_df))
        print(response.status_code)
