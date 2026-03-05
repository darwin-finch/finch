import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalSubscriptionsListRequest, CustomerPortalSubscriptionsListResponse, CustomerPortalSubscriptionsListSecurity } from "../models/operations/customerportalsubscriptionslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Subscriptions
 *
 * @remarks
 * List subscriptions of the authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalSubscriptionsList(client: PolarCore, security: CustomerPortalSubscriptionsListSecurity, request: CustomerPortalSubscriptionsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<CustomerPortalSubscriptionsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=customerPortalSubscriptionsList.d.ts.map