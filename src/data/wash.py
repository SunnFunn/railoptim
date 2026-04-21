import sys, os
import pymssql
import pickle
import redis
import pandas as pd

# adding paths to python modules to enable import
current = os.path.dirname(os.path.realpath(__file__))
parent = os.path.dirname(current)
sys.path.append(parent)

from utils import references


# функция получения данных о станциях промывки и передача их в Redis временное хранилище
# ------------------------------------------------------------------------------------------------------------------------------
def fetch_wash_stations(token, url, conndict, host, port, db, password, extime):
    # header = {'Authorization': 'Bearer {}'.format(token)}
    # wash_response = requests.get(url + "GetWashStationsData", headers=header)
    # wash_list = wash_response.json()

    # wash_list = references.wash_data

    # соединение и отправка запроса в БД SLP
    conn = pymssql.connect(server=conndict['server'],
                           user=(conndict['domain'] + conndict['user']),
                           password=conndict['password'],
                           database=conndict['database'])
    cursor = conn.cursor()

    stmt= \
    f'''
    SELECT RANKED.Firm, RANKED.RailWayWashDivision, RANKED.RailWayWash, RANKED.RailWayWashCode,
        RANKED.StationWash, RANKED.StationWashCode, RANKED.WashCapacity
    FROM
        (
        SELECT
            F.ShortName AS Firm, RP.Name AS RailWayWashDivision, R.ShortName AS RailWayWash, R.Code AS RailWayWashCode,
            S.Name AS StationWash, S.Code6 AS StationWashCode, AWS.CarCountPerDay AS WashCapacity,
            ROW_NUMBER() OVER (PARTITION BY AWS.calc_TargetFirmId, AWS.calc_StationCode6 \
                ORDER BY AWS.CreatedDateTime DESC) AS Rank
        FROM AgreementWashingStationView AWS (NOLOCK)
        JOIN NSI.Station (NOLOCK) S ON S.StationId = AWS.StationId
        JOIN Firm (NOLOCK) F ON F.FirmId = AWS.calc_TargetFirmId
        JOIN NSI.RailWay (NOLOCK) R ON R.RailWayId = S.RailWayId
        LEFT JOIN NSI.RailWayPart (NOLOCK) RP ON RP.RailWayPartId = S.RailWayPartId
        WHERE (AWS.calc_DateEnd IS NULL OR DATALENGTH(AWS.calc_DateEnd) = 0 OR LEN(AWS.calc_DateEnd) = 0)
        AND AWS.IsDeleted = 0
        ) RANKED
    WHERE RANKED.Rank = 1;
    '''
        
    cursor.execute(stmt)
    wash_list = [{'RailWayWashDivision': row[1],
                  'RailWayWash': row[2],
                  'RailWayWashCode': row[3],
                  'StationWash': row[4],
                  'StationWashCode': row[5],
                  'WashCapacity': row[6]} for row in cursor]

    cursor.close()
    conn.close()

    cols = ['RailWayWashDivision', 'RailWayWash', 'RailWayWashCode', 'StationWash', 'StationWashCode', 'WashCapacity']
    wash_list_df = pd.DataFrame(wash_list, columns=cols)
    wash_list_df_sorted = wash_list_df.sort_values(by='RailWayWash', ascending=True)

    r = redis.Redis(host=host, port=port, db=db, password=password)
    r.setex("wash", extime, pickle.dumps(wash_list))

    log_message = \
    f'''
    Washing stations data retrieved.
    Total number of washing stations: {len(wash_list)}
    Total washing capacity: {sum([w['WashCapacity'] for w in wash_list])}
    '''

    # отображение данных в логах airflow
    sys.stdout.write(wash_list_df_sorted.to_string())
    sys.stdout.write(log_message)