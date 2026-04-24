import pulp
from pulp import *
import pickle
import redis
import sys, os
# from cryptography.fernet import Fernet
from collections import defaultdict

# adding parent folder to python modules paths to enable import of utils module
current = os.path.dirname(os.path.realpath(__file__))
parent = os.path.dirname(current)
sys.path.append(parent)

from tasks import connection_dict_OPTIM
from tasks.data import opz

# min amount of assigment from middle supply station to middle-large demand stations
_ASSIGN_LOW_BOUND_ = 3
# min amount of assignment from bulk stations
_ASSIGN_BULK_BOUND_ = 3
# min size of middle demand station
_DEMAND_SIZE_BOUND_ = 5
# min size of middle supply station
_SUPPLY_SIZE_BOUND_ = 7
# min assignment qty to route demand station
_ROUTE_LOW_BOUND_ = 10

# ДМ ЗИ ограничения РЖД
# dmzi_dict = {"ГОР": 22.0, "КБШ": 110.0, "ЮУР": 50.0}
dmzi_dict = {"ВСБ": 10.0, "ЗАБ": 10.0, "ЗСБ": 150, "ГОР": 30.0, "КБШ": 100.0, "КРС": 10.0, "ПРВ": 50.0, "СВР": 10.0, "СЕВ": 12.0, "ЮВС": 150.0, "ЮУР": 50.0}


def optim(host, port, db, password, extime):
    r = redis.Redis(host=host, port=port, db=db, password=password)

    # cipher = Fernet(key)
    dummies = pickle.loads(r.get("dummy_nodes"))
    supply_node_list = pickle.loads(r.get("supply"))
    exempted_cars = pickle.loads(r.get("supply_exempted"))
    exempted_cars_dict = defaultdict(int)
    for car in exempted_cars:
        if car["Период"] == 1:
            exempted_cars_dict[car["Дорога назначения"]] += car["Количество вагонов"]
    demand_list = pickle.loads(r.get("demand"))
    costs_dict = pickle.loads(r.get("costs"))

    # supply_node_list.append(dummies[0])
    # demand_list.append(dummies[1])
    
    supply = r.get("supply_str")
    demand = r.get("demand_str")
    costs = r.get("costs_str")

    RailRoads_supply = [ele.split(" ")[0] for ele in supply.decode().split(",")[:-1]]
    RailRoads_demand = [ele.split(" ")[0] for ele in demand.decode().split(",")[:-1]]
    RailRoads_supply_qty = [float(ele.split(" ")[1][:-1]) for ele in supply.decode().split(",")[:-1]]
    RailRoads_demand_qty = [float(ele.split(" ")[1]) for ele in demand.decode().split(",")[:-1]]

    RailRoads_supply_no_dummy = RailRoads_supply[:-1]
    RailRoads_demand_no_dummy = RailRoads_demand[:-1]

    # print(dummies)
    # print(supply_node_list[1529])

    #-------------------------------------------------------------------------------------------------------------------------------------------
    # demand stations
    d_stations = defaultdict(list)
    for node in RailRoads_demand_no_dummy:
        d_node = demand_list[int(node)]
        d_code = d_node["Код станции погрузки"]
        d_stations[d_code].append(node)
    
    # DMZI demand stations
    dmzi_roads = defaultdict(list)
    for node in RailRoads_demand_no_dummy:
        d_node = demand_list[int(node)]
        d_road = d_node["Дорога погрузки"]
        if d_road in list(dmzi_dict.keys()):
            dmzi_roads[d_road].append(node)
    
    # print(dmzi_roads)
    
    # bulk supply stations
    bulks_nodes = defaultdict(list)
    bulks_qty = defaultdict(list)
    for node in RailRoads_supply_no_dummy:
        s_node = supply_node_list[int(node)]
        s_code = s_node["Код станции назначения"]
        s_qty = s_node["Количество вагонов"]
        if s_node["Признак массовой выгрузки"] == 1 and s_node["Период"] == 1:
            bulks_nodes[s_code].append(node)
            bulks_qty[s_code].append(s_qty)
    
    # middle supply stations
    s_stations = defaultdict(list)
    s_qty = defaultdict(int)
    for node in RailRoads_supply_no_dummy:
        s_node = supply_node_list[int(node)]
        if s_node["Период"] == 5 or s_node["Период"] == 0:
            continue
        if s_node["Признак массовой выгрузки"] == 1 and s_node["Период"] == 1:
            continue
        s_code = s_node["Код станции назначения"]
        if s_code in bulks_nodes.keys():
            continue
        s_stations[s_code].append(node)
        s_qty[s_code] += s_node["Количество вагонов"]
    s_qty_filtered = dict(filter(lambda item: item[1] >= _SUPPLY_SIZE_BOUND_, s_qty.items()))
    s_stations_filtered = dict(filter(lambda item: item[0] in s_qty_filtered.keys(), s_stations.items()))

    # supply route stations
    s_route_stations = defaultdict(list)
    s_route_qty = defaultdict(int)
    for node in RailRoads_supply_no_dummy:
    # for node in RailRoads_supply:
        s_node = supply_node_list[int(node)]
        if s_node["Период"] == 5 or s_node["Период"] == 0:
            continue
        s_code = s_node["Код станции назначения"]
        s_route_stations[s_code].append(node)
        s_route_qty[s_code] += s_node["Количество вагонов"]
    s_route_qty_filtered = dict(filter(lambda item: item[1] >= _ROUTE_LOW_BOUND_, s_route_qty.items()))
    s_route_stations_filtered = dict(filter(lambda item: item[0] in s_route_qty_filtered.keys(), s_route_stations.items()))

    print(s_route_stations_filtered, s_route_qty_filtered)
    
    # route demand stations nodes
    route_stations = defaultdict(list)
    route_qty = defaultdict(int)
    for node in RailRoads_demand_no_dummy:
    # for node in RailRoads_demand:
        route_node = demand_list[int(node)]
        if route_node["Тип отправки"] == "Маршрутная":
            route_code = route_node["Код станции погрузки"]
            route_stations[route_code].append(node)
            route_qty[route_code] += route_node["Количество вагонов"]
    
    print(route_stations, route_qty)
    
    # middle-large demand stations
    dml_stations = defaultdict(list)
    dml_qty = defaultdict(int)
    for node in RailRoads_demand_no_dummy:
        dml_node = demand_list[int(node)]
        if dml_node["Тип отправки"] == "Маршрутная":
            continue
        if dml_node["Период"] not in ("Отстой 1-5", "Промывка-1", "Промывка-5"):
            dml_code = dml_node["Код станции погрузки"]
            dml_stations[dml_code].append(node)
            dml_qty[dml_code] += dml_node["Количество вагонов"]
    dml_qty_filtered = dict(filter(lambda item: item[1] >= _DEMAND_SIZE_BOUND_, dml_qty.items()))
    dml_stations_filtered = dict(filter(lambda item: item[0] in dml_qty_filtered.keys(), dml_stations.items()))

    # The cost data is made into a dictionary
    c_list = [float(ele) for ele in costs.decode().split(",")[:-1]]
    costs_supply_demand = []
    for s_idx, _ in enumerate(RailRoads_supply):
        costs_demand = []
        for d_idx, d_node in enumerate(RailRoads_demand):
            costs_demand.append(c_list[s_idx*len(RailRoads_demand) + d_idx])
        costs_supply_demand.append(costs_demand)
    
    #-------------------------------------------------------------------------------------------------------------------------------------------
    costs = makeDict([RailRoads_supply, RailRoads_demand], costs_supply_demand, 0)

    # Creates the 'prob' variable to contain the problem data
    prob = LpProblem("RailCars Distribution Problem", LpMinimize)

    # Creates a list of tuples containing all the possible routes for transport
    Routes = [(s, d) for s in RailRoads_supply for d in RailRoads_demand]

    # A dictionary called 'Vars' is created to contain the referenced variables(the routes)
    vars = LpVariable.dicts("Route", (RailRoads_supply, RailRoads_demand), 0, None, LpInteger)
    bulk_vars = LpVariable.dicts("Bulk", (list(bulks_nodes.keys()), list(d_stations.keys())), cat="Binary")
    dml_vars = LpVariable.dicts("Dml", (list(dml_stations_filtered.keys()), list(s_stations_filtered.keys())), cat="Binary")
    route_vars = LpVariable.dicts("Routes", (list(route_stations.keys()), list(s_route_stations_filtered.keys())), cat="Binary")

    # Objective section
    #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # The objective function is added to 'prob' first
    # prob += (
    #     lpSum([vars[s][d] * costs[s][d] for (s, d) in Routes] + \
    #         [bulk_vars[b][d]*0 for b in bulks_nodes.keys() for d in d_stations.keys()] + \
    #         [dml_vars[dml][s]*0 for dml in dml_stations_filtered.keys() for s in s_stations_filtered.keys()] + \
    #         [route_vars[route][s]*0 for route in route_stations.keys() for s in s_route_stations_filtered.keys()]),
    #     "Sum_of_Transporting_Costs",
    # )

    prob += (
        lpSum([vars[s][d] * costs[s][d] for (s, d) in Routes] + \
            [dml_vars[dml][s]*0 for dml in dml_stations_filtered.keys() for s in s_stations_filtered.keys()] + \
            [route_vars[route][s]*0 for route in route_stations.keys() for s in s_route_stations_filtered.keys()]),
        "Sum_of_Transporting_Costs",
    )

    # prob += (
    #     lpSum([vars[s][d] * costs[s][d] for (s, d) in Routes] + \
    #         [bulk_vars[b][d]*0 for b in bulks_nodes.keys() for d in d_stations.keys()] + \
    #         [dml_vars[dml][s]*0 for dml in dml_stations_filtered.keys() for s in s_stations_filtered.keys()]),
    #     "Sum_of_Transporting_Costs",
    # )

    # prob += (
    #     lpSum([vars[s][d] * costs[s][d] for (s, d) in Routes]),
    #     "Sum_of_Transporting_Costs",
    # )

    # prob += (
    #     lpSum([vars[s][d] * costs[s][d] for (s, d) in Routes] + \
    #         [route_vars[route][s]*0 for route in route_stations.keys() for s in s_route_stations_filtered.keys()]),
    #     "Sum_of_Transporting_Costs",
    # )


    # Regular transportation constraints section
    #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # The supply maximum constraints are added to prob for each supply node (warehouse)
    for s, s_qty in zip(RailRoads_supply, RailRoads_supply_qty):
        prob += (
            lpSum([vars[s][d] for d in RailRoads_demand]) <= s_qty,
            f"Supply_{s}",
        )

    # The demand minimum constraints are added to prob for each demand node (bar)
    for d, d_qty in zip(RailRoads_demand, RailRoads_demand_qty):
        prob += (
                lpSum([vars[s][d] for s in RailRoads_supply]) >= d_qty,
                f"Demand_{d}",
            )
    
    # Bulk station section
    #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # bulk s_stations min assignments amount to one d_station constraints
    for s_code, s_nodes in bulks_nodes.items():
        for  d_code, d_nodes in d_stations.items():
            station_supply = sum(bulks_qty[s_code])
            prob += (
                lpSum([-vars[s][d] for s in s_nodes for d in d_nodes] + [min(_ASSIGN_BULK_BOUND_, station_supply)*bulk_vars[s_code][d_code]]) <=0,
                f"bulks_{s_code}_{d_code}_{1}",
            )
            prob += (
                lpSum([vars[s][d] for s in s_nodes for d in d_nodes] + [-100000*bulk_vars[s_code][d_code]]) <=0,
                f"bulks_{s_code}_{d_code}_{0}",
            )
    
    # # DMZI section
    # #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # dmzi_nodes = []
    # for node in dmzi_roads.values():
    #     dmzi_nodes += node
    
    # # print(dmzi_nodes)

    # # DMZI constraints
    # for d_road, d_nodes in dmzi_roads.items():
    #     d_qty = dmzi_dict[d_road]
    #     print(d_qty)
    #     prob += (
    #         lpSum([vars[s][d] for d in d_nodes for s in RailRoads_supply_no_dummy if supply_node_list[int(s)]["Период"] == 1]) <= d_qty,
    #         f"Dmzi_{d_road}",
    #     )

    # Middle-large demand constraints section
    # --------------------------------------------------------------------------------------------------------------------------------------------------------------
    # bulk middle-large d_stations min assignments amount from one s_station constraints
    for  s_code, s_nodes in s_stations_filtered.items():
        for dml_code, dml_nodes in dml_stations_filtered.items():
            prob += (
                lpSum([-vars[s][d] for s in s_nodes for d in dml_nodes] + [_ASSIGN_LOW_BOUND_*dml_vars[dml_code][s_code]]) <=0,
                f"dml_{s_code}_{dml_code}_{1}",
            )
            prob += (
                lpSum([vars[s][d] for s in s_nodes for d in dml_nodes] + [-100000*dml_vars[dml_code][s_code]]) <=0,
                f"dml_{s_code}_{dml_code}_{0}",
            )

    # Bulk routing constraints section
    #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # route d_stations min assignments amount from one s_station constraints
    for route_code, route_nodes in route_stations.items():
        for  s_code, s_nodes in s_route_stations_filtered.items():
            # prob += (
            #     lpSum([vars[s][d] for d in route_nodes for s in s_nodes]) >=_ROUTE_LOW_BOUND_,
            #     f"route_{s_code}_{route_code}_{0}",
            # )
            prob += (
                lpSum([-vars[s][d] for d in route_nodes for s in s_nodes] + [_ROUTE_LOW_BOUND_*route_vars[route_code][s_code]]) <=0,
                f"route_{s_code}_{route_code}_{1}",
            )
            prob += (
                lpSum([vars[s][d] for d in route_nodes for s in s_nodes] + [-100000*route_vars[route_code][s_code]]) <=0,
                f"route_{s_code}_{route_code}_{0}",
            )
    
    # # OPZ constraints section
    # # --------------------------------------------------------------------------------------------------------------------------------------------------------------
    # # opz_dict = {"СКВ": {"ГОР": 10, "КБШ": 53, "ПРВ": 57, "ЮВС": 42, "МСК": 35, "СКВ": 35},
    # #             "АЗР": {"КБШ": 9},
    # #             "ГОР": {"ГОР": 2},
    # #             "ЛАТ": {"КБШ":11},
    # #             "СВР": {"ЮУР": 15},
    # #             "ОКТ": {"ЮУР": 1, "СЕВ": 4},
    # # }
    # opz_assignments = opz.fetch_opz(connection_dict_OPTIM)
    # opz_dict = opz_assignments[0]

    # print(f"Assignments: {opz_assignments[0]}")
    # print(f"Assignment qty: {opz_assignments[1]}")
    # print(f"Exempted railcars: {exempted_cars_dict}")

    
    # for s_road, d_roads_dict in opz_dict.items():
    #     if s_road not in ("БЕЛ", "ОКТ", "СКВ"):
    #         continue

    #     # s_road_exempts = exempted_cars_dict[s_road]
    #     d_roads_qty = sum([int(qty) for qty in list(d_roads_dict.values()) if qty])
    #     s_road_qty = sum([node["Количество вагонов"] for node in supply_node_list if node["Дорога назначения"] == s_road and node["Период"] == 1])

    #     print(f"{s_road}   {s_road_qty}    {total_qty}")

    #     d_roads_updated = dict()
    #     if d_roads_qty > s_road_qty:
    #         factor = s_road_qty/d_roads_qty
    #         for d_road, d_road_qty in d_roads_dict.items():
    #             if not d_road_qty:
    #                 continue
    #             d_roads_updated[d_road] = int(d_road_qty*factor)
    #             d_roads_qty -= int(d_road_qty*factor)
    #     else:
    #         d_roads_updated = d_roads_dict
        
    #     print(f"Updated d_road: {d_roads_updated}")

    #     for d_road, d_road_qty in d_roads_updated.items():
    #         prob += (
    #                 lpSum([vars[s][d] for s in RailRoads_supply_no_dummy for d in RailRoads_demand_no_dummy if supply_node_list[int(s)]["Дорога назначения"] == s_road and demand_list[int(d)]["Дорога погрузки"] == d_road\
    #                     and supply_node_list[int(s)]["Период"] == 1]) == d_road_qty,
    #                 f"OPZ_{s_road}_{d_road}",
    #             )    
    
    # Solving section
    #--------------------------------------------------------------------------------------------------------------------------------------------------------------
    # The problem data is written to an .lp file
    # prob.writeLP("C:/Users/Алексей Третьяков/Desktop/TransportLP.lp")

    # The problem is solved using PuLP's choice of Solver
    prob.solve(pulp.PULP_CBC_CMD(msg=1))

    # # The status of the solution is printed to the screen
    print("Status:", LpStatus[prob.status])

    # Each of the variables is printed with it's resolved optimum value
    total_assignments = 0
    total_cost = 0
    assigns_str = ""
    for v in prob.variables():
        if str(v).split("_")[0] == "Bulk" or str(v).split("_")[0] == "Dml":
            continue 
        s_idx = str(v).split("_")[1]
        d_idx = str(v).split("_")[2]

        # if dummy[0] == "supply_dummy" and s_idx == dummy[1]:
        #     continue
        # if dummy[0] == "demand_dummy" and d_idx == dummy[1]:
        #     continue
        
        if int(s_idx) < len(RailRoads_supply) - 1:
            s_node = supply_node_list[int(s_idx)]
            s_code = s_node["Код станции назначения"]
        else:
            s_code = '0'
        
        if int(d_idx) < len(RailRoads_demand) - 1:
            d_node = demand_list[int(d_idx)]
            d_code = d_node["Код станции погрузки"]
        else:
            d_code = '0'

        if v.varValue != 0.0:
            assigns_str += s_idx + "_" + d_idx + "_" + str(int(v.varValue)) + ","
        
        s_period = s_node["Период"]
        if s_period == 5:
            continue

        total_assignments += v.varValue
        try:
            total_cost += costs_dict[(s_code, d_code)][2]
        except:
            total_cost += 1000000.0
        # if v.varValue != 0.0:
        #     assigns_str += s_idx + "_" + d_idx + "_" + str(int(v.varValue)) + ","

    r.setex("optim_output", extime, assigns_str)
    # print(assigns_str)

    log_message = f'''
    Total bulk stations: {len(bulks_nodes.keys())},
    Total bulk qty: {sum([sum(code) for code in bulks_qty.values()])},
    
    Total middle supply stations: {len(s_stations_filtered.keys())},
    Total middle supply qty: {sum([qty for qty in s_qty_filtered.values()])},

    Total middle demand stations: {len(dml_stations_filtered.keys())},
    Total middle demand stations qty: {sum([qty for qty in dml_qty_filtered.values()])},

    Total route supply stations: {len(s_route_stations_filtered.keys())},
    Total route supply stations qty: {sum([qty for qty in s_route_qty_filtered.values()])},

    Total route demand stations: {len(route_stations.keys())},
    Total route demand stations qty: {sum([qty for qty in route_qty.values()])},

    Total Cost of Transportation = {value(prob.objective)},
    Cost cleaned of dummy assignments: {total_cost},
    Total Assignments = {total_assignments}.
    '''
    sys.stdout.write(log_message)

# if __name__ == "__main__":
#     r_host = "0.0.0.0"
#     r_port = 6380
#     r_db = 0
#     password = os.getenv("PASSWORD")
#     optim(host=r_host, port=r_port, db=r_db, password="78mtS@", extime=3600)