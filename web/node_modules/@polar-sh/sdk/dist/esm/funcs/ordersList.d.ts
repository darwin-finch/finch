import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { OrdersListRequest, OrdersListResponse } from "../models/operations/orderslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Orders
 *
 * @remarks
 * List orders.
 *
 * **Scopes**: `orders:read`
 */
export declare function ordersList(client: PolarCore, request: OrdersListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<OrdersListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=ordersList.d.ts.map