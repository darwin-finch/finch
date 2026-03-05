import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerSubscription } from "../models/components/customersubscription.js";
import { AlreadyCanceledSubscription } from "../models/errors/alreadycanceledsubscription.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalSubscriptionsUpdateRequest, CustomerPortalSubscriptionsUpdateSecurity } from "../models/operations/customerportalsubscriptionsupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Subscription
 *
 * @remarks
 * Update a subscription of the authenticated customer.
 *
 * **Scopes**: `customer_portal:write`
 */
export declare function customerPortalSubscriptionsUpdate(client: PolarCore, security: CustomerPortalSubscriptionsUpdateSecurity, request: CustomerPortalSubscriptionsUpdateRequest, options?: RequestOptions): APIPromise<Result<CustomerSubscription, AlreadyCanceledSubscription | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalSubscriptionsUpdate.d.ts.map