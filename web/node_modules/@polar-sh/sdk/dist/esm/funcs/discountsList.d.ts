import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { DiscountsListRequest, DiscountsListResponse } from "../models/operations/discountslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Discounts
 *
 * @remarks
 * List discounts.
 *
 * **Scopes**: `discounts:read` `discounts:write`
 */
export declare function discountsList(client: PolarCore, request: DiscountsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<DiscountsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=discountsList.d.ts.map